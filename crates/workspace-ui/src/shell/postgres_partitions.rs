//! Vim Monitor attachment form. Inputs become typed bound data, never SQL.
use super::*;
use sift_protocol::{
    PostgresObjectAction, PostgresPartitionBound as Bound, PostgresPartitionValue as BoundValue,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Mode {
    Range,
    List,
    Hash,
    Default,
}

impl Mode {
    fn label(self) -> &'static str {
        match self {
            Self::Range => "Range",
            Self::List => "List",
            Self::Hash => "Hash",
            Self::Default => "Default",
        }
    }
    fn next(self) -> Self {
        match self {
            Self::Range => Self::List,
            Self::List => Self::Hash,
            Self::Hash => Self::Default,
            Self::Default => Self::Range,
        }
    }
}

pub(super) struct PartitionAttachState {
    pub active: bool,
    pub parent_schema: Entity<TextInput>,
    pub parent: Entity<TextInput>,
    pub child_schema: Entity<TextInput>,
    pub child: Entity<TextInput>,
    pub lower: Entity<TextInput>,
    pub upper: Entity<TextInput>,
    pub mode: Mode,
}

impl PartitionAttachState {
    pub fn new(window: &mut Window, cx: &mut Context<WorkspaceShell>) -> Self {
        let form = Self {
            active: false,
            parent_schema: cx.new(|cx| {
                TextInput::new("public", "Parent schema", cx).aria_label("Parent schema")
            }),
            parent: cx.new(|cx| {
                TextInput::new("", "Partitioned parent table", cx).aria_label("Parent table")
            }),
            child_schema: cx
                .new(|cx| TextInput::new("public", "Child schema", cx).aria_label("Child schema")),
            child: cx.new(|cx| {
                TextInput::new("", "Existing ordinary child table", cx).aria_label("Child table")
            }),
            lower: cx.new(|cx| {
                TextInput::new("", "Bound values or modulus", cx)
                    .aria_label("Lower bound, list values or hash modulus")
            }),
            upper: cx.new(|cx| {
                TextInput::new("", "Upper values or remainder", cx)
                    .aria_label("Upper bound or hash remainder")
            }),
            mode: Mode::Range,
        };
        for input in form.inputs() {
            cx.subscribe_in(
                input,
                window,
                |shell, _, event: &TextInputEvent, window, cx| {
                    if *event == TextInputEvent::Submitted && shell.partition_attach.active {
                        shell.preview_partition_attach(window, cx);
                    }
                },
            )
            .detach();
        }
        form
    }

    fn inputs(&self) -> [&Entity<TextInput>; 6] {
        [
            &self.parent_schema,
            &self.parent,
            &self.child_schema,
            &self.child,
            &self.lower,
            &self.upper,
        ]
    }

    fn request(&self, cx: &App) -> Result<PostgresObjectAction, String> {
        let names = [
            &self.parent_schema,
            &self.parent,
            &self.child_schema,
            &self.child,
        ]
        .map(|input| input.read(cx).text().trim().to_owned());
        if names
            .iter()
            .any(|name| name.is_empty() || name.len() > 63 || name.chars().any(char::is_control))
        {
            return Err(
                "Schema and table names must be 1..=63 bytes without control characters".into(),
            );
        }
        let lower = self.lower.read(cx).text();
        let upper = self.upper.read(cx).text();
        let bound = match self.mode {
            Mode::Range => Bound::Range {
                from: parse_values(lower)?,
                to: parse_values(upper)?,
            },
            Mode::List => Bound::List {
                values: parse_values(lower)?,
            },
            Mode::Hash => Bound::Hash {
                modulus: lower
                    .trim()
                    .parse()
                    .map_err(|_| "Hash modulus must be an integer")?,
                remainder: upper
                    .trim()
                    .parse()
                    .map_err(|_| "Hash remainder must be an integer")?,
            },
            Mode::Default => Bound::Default,
        };
        let [parent_schema, parent, child_schema, child] = names;
        Ok(PostgresObjectAction::AttachPartition {
            parent_schema,
            parent,
            child_schema,
            child,
            bound,
        })
    }
}

/// CSV-style quoting preserves literal NULL/MINVALUE/MAXVALUE and commas.
fn parse_values(input: &str) -> Result<Vec<BoundValue>, String> {
    if input.len() > 32 * 1024 || input.trim().is_empty() {
        return Err("Enter bounded partition values".into());
    }
    let mut chars = input.chars().peekable();
    let mut result = Vec::new();
    loop {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        let quoted = chars.peek() == Some(&'"');
        let mut value = String::new();
        if quoted {
            chars.next();
            loop {
                match chars.next() {
                    Some('"') if chars.peek() == Some(&'"') => {
                        chars.next();
                        value.push('"');
                    }
                    Some('"') => break,
                    Some(c) => value.push(c),
                    None => return Err("Unclosed quoted partition value".into()),
                }
            }
            while chars.peek().is_some_and(|c| c.is_whitespace()) {
                chars.next();
            }
            if !matches!(chars.peek(), None | Some(',')) {
                return Err("Separate quoted values with commas".into());
            }
        } else {
            while chars.peek().is_some_and(|c| *c != ',') {
                let c = chars.next().unwrap();
                if c == '"' {
                    return Err("Quote the entire partition value".into());
                }
                value.push(c);
            }
            value = value.trim().to_owned();
            if value.is_empty() {
                return Err("Quote empty text as \"\"".into());
            }
        }
        if value.len() > 256 || value.chars().any(char::is_control) {
            return Err("Partition values must be at most 256 bytes without controls".into());
        }
        let item = if quoted {
            BoundValue::Literal(value)
        } else {
            match value.to_ascii_uppercase().as_str() {
                "NULL" => BoundValue::Null,
                "MINVALUE" => BoundValue::MinValue,
                "MAXVALUE" => BoundValue::MaxValue,
                _ => BoundValue::Literal(value),
            }
        };
        result.push(item);
        if result.len() > 100 {
            return Err("Use at most 100 partition values".into());
        }
        if chars.next().is_none() {
            break;
        }
        if chars.peek().is_none() {
            return Err("Missing value after comma".into());
        }
    }
    Ok(result)
}

impl WorkspaceShell {
    pub(super) fn begin_partition_attach(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.database_monitor.object_action_request().loading() {
            return;
        }
        let selected = self
            .database_monitor
            .partitions()
            .get(self.database_monitor.objects_selected())
            .cloned();
        let (schema, table) = selected.map_or(("public".to_owned(), String::new()), |item| {
            (item.parent_schema, item.parent)
        });
        for (input, value) in [
            (&self.partition_attach.parent_schema, schema.clone()),
            (&self.partition_attach.parent, table),
            (&self.partition_attach.child_schema, schema),
            (&self.partition_attach.child, String::new()),
            (&self.partition_attach.lower, String::new()),
            (&self.partition_attach.upper, String::new()),
        ] {
            input.update(cx, |input, cx| input.set_text(value, cx));
        }
        self.partition_attach.mode = Mode::Range;
        self.partition_attach.active = true;
        self.database_monitor.clear_object_preview();
        self.partition_attach
            .parent
            .focus_handle(cx)
            .focus(window, cx);
        cx.notify();
    }

    pub(super) fn preview_partition_attach(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.partition_attach.request(cx) {
            Ok(action) => {
                self.partition_attach.active = false;
                self.automation_focus_handle.focus(window, cx);
                self.preview_postgres_object(action, cx);
            }
            Err(message) => {
                self.database_monitor.fail_object_action(message);
                cx.notify();
            }
        }
    }

    pub(super) fn partition_attach_key(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let editing = self
            .partition_attach
            .inputs()
            .iter()
            .any(|input| input.focus_handle(cx).is_focused(window));
        match event.keystroke.key.as_str() {
            "enter" | "p" if !editing || event.keystroke.key == "enter" => {
                self.preview_partition_attach(window, cx)
            }
            "m" if !editing => {
                self.partition_attach.mode = self.partition_attach.mode.next();
                cx.notify();
            }
            "i" if !editing => self
                .partition_attach
                .parent
                .focus_handle(cx)
                .focus(window, cx),
            "escape" if editing => self.automation_focus_handle.focus(window, cx),
            "escape" => {
                self.partition_attach.active = false;
                cx.notify();
            }
            _ => return,
        }
        cx.stop_propagation();
    }
}

pub(super) fn render_form(shell: &WorkspaceShell, cx: &mut Context<WorkspaceShell>) -> AnyElement {
    let form = &shell.partition_attach;
    let field = |label: &'static str, input: &Entity<TextInput>| {
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_1()
            .child(label)
            .child(input.clone())
    };
    div().id("partition-attach-form").debug_selector(|| "partition-attach-form".into())
        .flex_1().min_h_0().overflow_y_scroll().flex().flex_col().gap_2().p_3()
        .child(div().flex().gap_2().child(field("Parent schema", &form.parent_schema)).child(field("Parent table", &form.parent)))
        .child(div().flex().gap_2().child(field("Child schema", &form.child_schema)).child(field("Existing child table", &form.child)))
        .child(div().flex().gap_2().children([Mode::Range, Mode::List, Mode::Hash, Mode::Default].into_iter().map(|mode| {
            Button::new(("partition-mode", mode as usize), mode.label())
                .tone(if form.mode == mode { ButtonTone::Neutral } else { ButtonTone::Ghost })
                .on_click(cx.listener(move |shell, _, window, cx| { shell.partition_attach.mode = mode; shell.automation_focus_handle.focus(window, cx); cx.notify(); }))
        })))
        .when(form.mode != Mode::Default, |panel| panel.child(
            div().flex().gap_2()
                .child(field(match form.mode { Mode::Range => "From values", Mode::List => "List values", _ => "Modulus" }, &form.lower))
                .when(form.mode != Mode::List, |row| row.child(field(if form.mode == Mode::Hash { "Remainder" } else { "To values" }, &form.upper)))
        ))
        .child(div().text_xs().child("Comma-separated values; double-quote text containing commas or literal NULL/MINVALUE/MAXVALUE. Tab moves fields. Esc leaves editing; m changes mode."))
        .child(div().flex().gap_2()
            .child(Button::new("partition-attach-preview", "Preview attachment").on_click(cx.listener(|shell, _, window, cx| shell.preview_partition_attach(window, cx))))
            .child(Button::new("partition-attach-cancel", "Cancel").on_click(cx.listener(|shell, _, window, cx| { shell.partition_attach.active = false; shell.automation_focus_handle.focus(window, cx); cx.notify(); }))))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quoted_values_preserve_reserved_text_and_commas() {
        let values = parse_values("NULL, \"NULL\", MINVALUE, \"a,b\", \"a\"\"b\", \"\"").unwrap();
        assert!(matches!(values[0], BoundValue::Null));
        assert!(matches!(&values[1], BoundValue::Literal(value) if value == "NULL"));
        assert!(matches!(values[2], BoundValue::MinValue));
        assert!(matches!(&values[3], BoundValue::Literal(value) if value == "a,b"));
        assert!(matches!(&values[4], BoundValue::Literal(value) if value == "a\"b"));
        assert!(matches!(&values[5], BoundValue::Literal(value) if value.is_empty()));
        for input in ["", "a,", "\"unclosed", "\"a\"b", ",b"] {
            assert!(parse_values(input).is_err());
        }
    }
}
