//! Native transcript Markdown. Content is parsed as text, never HTML or code.
use super::*;
use crate::editor::{language_text_runs, EditorLanguage};
use gpui::{FontStyle, FontWeight, StyledText, TextRun};
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct InlineStyle {
    strong: bool,
    emphasis: bool,
    strike: bool,
    code: bool,
    link: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct RichText {
    text: String,
    spans: Vec<(Range<usize>, InlineStyle)>,
}

impl RichText {
    fn append(&mut self, text: &str, style: InlineStyle) {
        let start = self.text.len();
        self.text.push_str(text);
        if start != self.text.len() {
            self.spans.push((start..self.text.len(), style));
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Block {
    Text {
        content: RichText,
        heading: Option<u8>,
        quote: bool,
        prefix: String,
        depth: usize,
    },
    Code {
        language: String,
        text: String,
    },
    Table(Vec<Vec<RichText>>),
    Rule,
}

fn parse(markdown: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut content = RichText::default();
    let mut styles = vec![InlineStyle::default()];
    let mut heading = None;
    let mut quote = 0usize;
    let mut lists: Vec<Option<u64>> = Vec::new();
    let mut prefix = String::new();
    let mut code: Option<(String, String)> = None;
    let mut table: Option<Vec<Vec<RichText>>> = None;
    let mut row = Vec::new();
    let flush = |blocks: &mut Vec<Block>,
                 content: &mut RichText,
                 heading,
                 quote,
                 prefix: &mut String,
                 depth| {
        if !content.text.is_empty() {
            blocks.push(Block::Text {
                content: std::mem::take(content),
                heading,
                quote,
                prefix: std::mem::take(prefix),
                depth,
            });
        }
    };
    for event in Parser::new_ext(
        markdown,
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS,
    ) {
        match event {
            Event::Start(Tag::CodeBlock(kind)) => {
                flush(
                    &mut blocks,
                    &mut content,
                    heading,
                    quote > 0,
                    &mut prefix,
                    lists.len(),
                );
                let language = match kind {
                    CodeBlockKind::Fenced(info) => {
                        info.split_whitespace().next().unwrap_or("text").to_owned()
                    }
                    CodeBlockKind::Indented => "text".into(),
                };
                code = Some((language, String::new()));
            }
            Event::End(TagEnd::CodeBlock) => {
                if let Some((language, text)) = code.take() {
                    blocks.push(Block::Code { language, text });
                }
            }
            Event::Text(text) | Event::Html(text) | Event::InlineHtml(text) => {
                if let Some((_, code)) = &mut code {
                    code.push_str(&text);
                } else {
                    content.append(&text, styles.last().cloned().unwrap_or_default());
                }
            }
            Event::Code(text) => {
                let mut style = styles.last().cloned().unwrap_or_default();
                style.code = true;
                content.append(&text, style);
            }
            Event::Start(Tag::Strong | Tag::Emphasis | Tag::Strikethrough | Tag::Link { .. }) => {
                let mut style = styles.last().cloned().unwrap_or_default();
                if let Event::Start(tag) = event {
                    match tag {
                        Tag::Strong => style.strong = true,
                        Tag::Emphasis => style.emphasis = true,
                        Tag::Strikethrough => style.strike = true,
                        Tag::Link { dest_url, .. } => style.link = safe_link(&dest_url),
                        _ => {}
                    }
                }
                styles.push(style);
            }
            Event::End(
                TagEnd::Strong | TagEnd::Emphasis | TagEnd::Strikethrough | TagEnd::Link,
            ) => {
                if styles.len() > 1 {
                    styles.pop();
                }
            }
            Event::Start(Tag::Heading { level, .. }) => heading = Some(level as u8),
            Event::End(TagEnd::Heading(_)) => {
                flush(
                    &mut blocks,
                    &mut content,
                    heading,
                    quote > 0,
                    &mut prefix,
                    lists.len(),
                );
                heading = None;
            }
            Event::Start(Tag::BlockQuote(_)) => quote += 1,
            Event::End(TagEnd::BlockQuote(_)) => {
                flush(
                    &mut blocks,
                    &mut content,
                    heading,
                    true,
                    &mut prefix,
                    lists.len(),
                );
                quote = quote.saturating_sub(1);
            }
            Event::Start(Tag::List(start)) => {
                flush(
                    &mut blocks,
                    &mut content,
                    heading,
                    quote > 0,
                    &mut prefix,
                    lists.len(),
                );
                lists.push(start);
            }
            Event::End(TagEnd::List(_)) => {
                flush(
                    &mut blocks,
                    &mut content,
                    heading,
                    quote > 0,
                    &mut prefix,
                    lists.len(),
                );
                lists.pop();
            }
            Event::Start(Tag::Item) => {
                flush(
                    &mut blocks,
                    &mut content,
                    heading,
                    quote > 0,
                    &mut prefix,
                    lists.len(),
                );
                prefix = match lists.last_mut() {
                    Some(Some(next)) => {
                        let label = format!("{next}.");
                        *next += 1;
                        label
                    }
                    _ => "•".into(),
                };
            }
            Event::End(TagEnd::Item | TagEnd::Paragraph | TagEnd::HtmlBlock) => {
                if table.is_none() {
                    flush(
                        &mut blocks,
                        &mut content,
                        heading,
                        quote > 0,
                        &mut prefix,
                        lists.len(),
                    );
                }
            }
            Event::TaskListMarker(checked) => prefix = if checked { "☑" } else { "☐" }.into(),
            Event::SoftBreak | Event::HardBreak => {
                content.append("\n", styles.last().cloned().unwrap_or_default())
            }
            Event::Start(Tag::Table(_)) => {
                flush(
                    &mut blocks,
                    &mut content,
                    heading,
                    quote > 0,
                    &mut prefix,
                    lists.len(),
                );
                table = Some(Vec::new());
            }
            Event::End(TagEnd::TableCell) => row.push(std::mem::take(&mut content)),
            Event::End(TagEnd::TableHead | TagEnd::TableRow) => {
                if let Some(table) = &mut table {
                    table.push(std::mem::take(&mut row));
                }
            }
            Event::End(TagEnd::Table) => {
                if let Some(table) = table.take() {
                    blocks.push(Block::Table(table));
                }
            }
            Event::Rule => {
                flush(
                    &mut blocks,
                    &mut content,
                    heading,
                    quote > 0,
                    &mut prefix,
                    lists.len(),
                );
                blocks.push(Block::Rule);
            }
            _ => {}
        }
    }
    flush(
        &mut blocks,
        &mut content,
        heading,
        quote > 0,
        &mut prefix,
        lists.len(),
    );
    blocks
}

fn safe_link(destination: &str) -> Option<String> {
    let url = url::Url::parse(destination).ok()?;
    matches!(url.scheme(), "http" | "https").then(|| url.into())
}

fn rich_text(
    id: String,
    content: RichText,
    theme: sift_ui::Theme,
    bold: bool,
    selection: SelectionBlock,
) -> AnyElement {
    let mut links = Vec::new();
    let runs = content
        .spans
        .into_iter()
        .map(|(range, style)| {
            let mut font = gpui::font(if style.code {
                "monospace"
            } else {
                "sans-serif"
            });
            if style.strong || bold {
                font.weight = FontWeight::BOLD;
            }
            if style.emphasis {
                font.style = FontStyle::Italic;
            }
            if let Some(link) = style.link {
                links.push((range.clone(), link));
            }
            TextRun {
                len: range.len(),
                font,
                color: if style.code {
                    theme.colors.syntax_string
                } else if links.last().is_some_and(|(link, _)| *link == range) {
                    theme.colors.accent
                } else {
                    theme.colors.text
                },
                background_color: style.code.then_some(theme.colors.surface),
                underline: None,
                strikethrough: style.strike.then_some(gpui::StrikethroughStyle {
                    color: Some(theme.colors.muted_text),
                    thickness: px(1.),
                }),
            }
        })
        .collect();
    selection
        .element(id, StyledText::new(content.text).with_runs(runs))
        .links(links)
        .into_any_element()
}

pub(super) struct AiResponseMenu {
    pub(super) position: gpui::Point<Pixels>,
    pub(super) plain: String,
    pub(super) markdown: String,
    pub(super) selected: Option<String>,
}

#[derive(Clone)]
struct SelectionBlock {
    document: String,
    text: SharedString,
    range: Range<usize>,
    selection: Rc<RefCell<sift_ui::TextSelection>>,
    focus: FocusHandle,
}
impl SelectionBlock {
    fn element(self, id: String, text: StyledText) -> sift_ui::SelectableText {
        sift_ui::SelectableText::new(
            id,
            text,
            self.document,
            self.text,
            self.range,
            self.selection,
            self.focus,
        )
    }
}

fn plain_text(block: &Block) -> String {
    match block {
        Block::Text {
            content, prefix, ..
        } => {
            if prefix.is_empty() {
                content.text.clone()
            } else {
                format!("{prefix} {}", content.text)
            }
        }
        Block::Code { text, .. } => text.clone(),
        Block::Rule => "---".into(),
        Block::Table(rows) => rows
            .iter()
            .map(|row| {
                row.iter()
                    .map(|cell| cell.text.as_str())
                    .collect::<Vec<_>>()
                    .join("\t")
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

impl WorkspaceShell {
    pub(super) fn render_ai_code(
        &self,
        id: &str,
        language: &str,
        text: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selection = SelectionBlock {
            document: format!("code-{id}"),
            text: text.to_owned().into(),
            range: 0..text.len(),
            selection: self.ai.text_selection.clone(),
            focus: self.ai.transcript_focus.clone(),
        };
        self.render_ai_code_selection(id, language, text, Some(selection), cx)
    }

    fn render_ai_code_selection(
        &self,
        id: &str,
        language: &str,
        text: &str,
        selection: Option<SelectionBlock>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let language_kind = match language.to_ascii_lowercase().as_str() {
            "sql" | "postgres" | "postgresql" | "pgsql" | "tsql" | "t-sql" | "sqlserver"
            | "sql-server" | "mssql" | "sqlite" | "mysql" => EditorLanguage::Sql,
            "json" | "jsonc" => EditorLanguage::Json,
            "toml" => EditorLanguage::Toml,
            "md" | "markdown" => EditorLanguage::Markdown,
            _ => EditorLanguage::PlainText,
        };
        let runs = text
            .split_inclusive('\n')
            .flat_map(|line| {
                language_text_runs(line, gpui::font("monospace"), theme, language_kind)
            })
            .collect();
        let copy = text.to_owned();
        div()
            .id(format!("ai-code-{id}"))
            .w_full()
            .flex()
            .flex_col()
            .min_w_0()
            .border_1()
            .border_color(theme.colors.subtle_border)
            .rounded_md()
            .bg(theme.colors.surface)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_2()
                    .py_1()
                    .text_xs()
                    .text_color(theme.colors.muted_text)
                    .child(language.to_owned())
                    .child(
                        IconButton::new(format!("ai-copy-code-{id}"), IconName::Copy, "Copy code")
                            .debug_selector(format!("ai-copy-code-{id}"))
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(gpui::ClipboardItem::new_string(copy.clone()))
                            }),
                    ),
            )
            .child(
                div()
                    .id(format!("ai-code-scroll-{id}"))
                    .w_full()
                    .min_w_0()
                    .overflow_x_scroll()
                    .p_2()
                    .font_family("monospace")
                    .text_sm()
                    .whitespace_nowrap()
                    .child({
                        let styled = StyledText::new(text.to_owned()).with_runs(runs);
                        match selection {
                            Some(selection) => selection
                                .element(format!("ai-selectable-code-{id}"), styled)
                                .into_any_element(),
                            None => styled.into_any_element(),
                        }
                    }),
            )
            .into_any_element()
    }

    pub(super) fn render_ai_markdown(
        &self,
        id: &str,
        markdown: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let blocks = parse(markdown);
        let plain = blocks
            .iter()
            .map(plain_text)
            .collect::<Vec<_>>()
            .join("\n\n");
        let document_text: SharedString = plain.clone().into();
        let document = id.to_owned();
        let markdown = markdown.to_owned();
        let menu_document = document.clone();
        let mut offset = 0;
        div()
            .id(format!("ai-markdown-{id}"))
            .debug_selector({
                let id = id.to_owned();
                move || format!("ai-markdown-{id}")
            })
            .w_full()
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |shell, event: &gpui::MouseDownEvent, window, cx| {
                    shell.ai.response_menu = Some(AiResponseMenu {
                        position: event.position,
                        plain: plain.clone(),
                        markdown: markdown.clone(),
                        selected: shell
                            .ai
                            .text_selection
                            .borrow()
                            .selected_text_in(&menu_document),
                    });
                    shell.ai.menu_expanded = false;
                    shell.ai.thread_picker_expanded = false;
                    shell.ai.popup_selected = 0;
                    shell.ai.transcript_focus.focus(window, cx);
                    cx.notify();
                    cx.stop_propagation();
                }),
            )
            .min_w_0()
            .flex()
            .flex_col()
            .gap_1()
            .children(blocks.into_iter().enumerate().map(|(index, block)| {
                let id = format!("{id}-{index}");
                let block_start = offset;
                offset += plain_text(&block).len() + 2;
                let selection = |range| SelectionBlock {
                    document: document.clone(),
                    text: document_text.clone(),
                    range,
                    selection: self.ai.text_selection.clone(),
                    focus: self.ai.transcript_focus.clone(),
                };
                match block {
                    Block::Text {
                        content,
                        heading,
                        quote,
                        prefix,
                        depth,
                    } => {
                        let start = block_start
                            + if prefix.is_empty() {
                                0
                            } else {
                                prefix.len() + 1
                            };
                        let selected = selection(start..start + content.text.len());
                        div()
                            .w_full()
                            .flex()
                            .gap_2()
                            .min_w_0()
                            .pl(px(depth.saturating_sub(1) as f32 * 14.))
                            .when(quote, |view| {
                                view.border_l_2()
                                    .border_color(theme.colors.subtle_border)
                                    .pl_2()
                            })
                            .when(!prefix.is_empty(), |view| {
                                view.child(div().flex_none().child(prefix))
                            })
                            .child(
                                div()
                                    .debug_selector({
                                        let id = id.clone();
                                        move || format!("ai-text-{id}")
                                    })
                                    .min_w_0()
                                    .flex_1()
                                    .whitespace_normal()
                                    .when(heading.is_some(), |view| {
                                        view.text_size(px(match heading {
                                            Some(1) => 20.,
                                            Some(2) => 18.,
                                            _ => 16.,
                                        }))
                                    })
                                    .child(rich_text(
                                        id,
                                        content,
                                        theme,
                                        heading.is_some(),
                                        selected,
                                    )),
                            )
                            .into_any_element()
                    }
                    Block::Code { language, text } => self.render_ai_code_selection(
                        &id,
                        &language,
                        &text,
                        Some(selection(block_start..block_start + text.len())),
                        cx,
                    ),
                    Block::Rule => div()
                        .h(px(1.))
                        .w_full()
                        .bg(theme.colors.subtle_border)
                        .into_any_element(),
                    Block::Table(rows) => {
                        let mut cell_offset = block_start;
                        div()
                            .id(format!("ai-table-{id}"))
                            .overflow_x_scroll()
                            .flex()
                            .flex_col()
                            .border_1()
                            .border_color(theme.colors.subtle_border)
                            .rounded_md()
                            .children(rows.into_iter().enumerate().map(|(row_index, row)| {
                                div()
                                    .flex()
                                    .when(row_index == 0, |view| view.bg(theme.colors.surface))
                                    .children(row.into_iter().enumerate().map(|(column, cell)| {
                                        let selected =
                                            selection(cell_offset..cell_offset + cell.text.len());
                                        cell_offset += cell.text.len() + 1;
                                        div()
                                            .w(px(160.))
                                            .flex_none()
                                            .p_2()
                                            .whitespace_normal()
                                            .border_b_1()
                                            .border_color(theme.colors.subtle_border)
                                            .child(rich_text(
                                                format!("{id}-{row_index}-{column}"),
                                                cell,
                                                theme,
                                                row_index == 0,
                                                selected,
                                            ))
                                    }))
                            }))
                            .into_any_element()
                    }
                }
            }))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transcript_preserves_code_and_styles_nested_unicode_content() {
        let blocks = parse("## Résumé\n\n**Bold and _italic_** with `a < b`.\n\n```sql\nSELECT 'é', 42; -- keep\n```\n\n- [x] Done\n- Next\n\n| Name | Value |\n| --- | --- |\n| a | **b** |\n\n<script>alert(1)</script>");
        assert!(
            matches!(&blocks[0], Block::Text { heading: Some(2), content, .. } if content.text == "Résumé")
        );
        assert!(
            matches!(&blocks[1], Block::Text { content, .. } if content.spans.iter().any(|(_, style)| style.strong && style.emphasis))
        );
        assert!(
            matches!(&blocks[2], Block::Code { language, text } if language == "sql" && text == "SELECT 'é', 42; -- keep\n")
        );
        assert!(blocks
            .iter()
            .any(|block| matches!(block, Block::Text { prefix, .. } if prefix == "☑")));
        assert!(blocks.iter().any(
            |block| matches!(block, Block::Table(rows) if rows.len() == 2 && rows[1][1].text == "b")
        ));
        assert!(blocks.iter().any(|block| matches!(block, Block::Text { content, .. } if content.text.contains("<script>"))));
        assert!(safe_link("javascript:alert(1)").is_none());
        assert!(safe_link("file:///tmp/private").is_none());
        assert!(safe_link("https://example.com").is_some());
        assert!(
            matches!(&parse("```sql\nSELECT 1")[0], Block::Code { text, .. } if text == "SELECT 1")
        );
    }
}
