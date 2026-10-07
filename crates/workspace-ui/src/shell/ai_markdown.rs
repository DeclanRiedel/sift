//! Native transcript Markdown. Content is parsed as text, never HTML or code.
use super::*;
use crate::editor::{language_text_runs, EditorLanguage};
use gpui::{FontStyle, FontWeight, InteractiveText, StyledText, TextRun};
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

fn rich_text(id: String, content: RichText, theme: sift_ui::Theme, bold: bool) -> AnyElement {
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
    let ranges = links.iter().map(|(range, _)| range.clone()).collect();
    InteractiveText::new(id, StyledText::new(content.text).with_runs(runs))
        .on_click(ranges, move |index, _, cx| {
            if let Some((_, link)) = links.get(index) {
                cx.open_url(link);
            }
        })
        .into_any_element()
}

impl WorkspaceShell {
    pub(super) fn render_ai_code(
        &self,
        id: &str,
        language: &str,
        text: &str,
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
                    .overflow_x_scroll()
                    .p_2()
                    .font_family("monospace")
                    .text_sm()
                    .whitespace_nowrap()
                    .child(StyledText::new(text.to_owned()).with_runs(runs)),
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
        div()
            .id(format!("ai-markdown-{id}"))
            .min_w_0()
            .flex()
            .flex_col()
            .gap_1()
            .children(
                parse(markdown)
                    .into_iter()
                    .enumerate()
                    .map(|(index, block)| {
                        let id = format!("{id}-{index}");
                        match block {
                            Block::Text {
                                content,
                                heading,
                                quote,
                                prefix,
                                depth,
                            } => div()
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
                                        .child(rich_text(id, content, theme, heading.is_some())),
                                )
                                .into_any_element(),
                            Block::Code { language, text } => {
                                self.render_ai_code(&id, &language, &text, cx)
                            }
                            Block::Rule => div()
                                .h(px(1.))
                                .w_full()
                                .bg(theme.colors.subtle_border)
                                .into_any_element(),
                            Block::Table(rows) => div()
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
                                        .children(row.into_iter().enumerate().map(
                                            |(column, cell)| {
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
                                                    ))
                                            },
                                        ))
                                }))
                                .into_any_element(),
                        }
                    }),
            )
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
