use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LineNumberOptions {
    pub number: bool,
    pub relativenumber: bool,
}

impl Default for LineNumberOptions {
    fn default() -> Self {
        Self {
            number: true,
            relativenumber: false,
        }
    }
}

impl gpui::Global for LineNumberOptions {}

impl LineNumberOptions {
    pub fn label(
        self,
        source_line: usize,
        cursor_line: usize,
        first_fragment: bool,
    ) -> Option<usize> {
        if !first_fragment {
            return None;
        }
        if self.relativenumber {
            Some(if self.number && source_line == cursor_line {
                source_line + 1
            } else {
                source_line.abs_diff(cursor_line)
            })
        } else {
            self.number.then_some(source_line + 1)
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct LineNumberOverrides(pub [Option<bool>; 2]);

impl LineNumberOverrides {
    pub fn resolve(self, defaults: LineNumberOptions) -> LineNumberOptions {
        LineNumberOptions {
            number: self.0[0].unwrap_or(defaults.number),
            relativenumber: self.0[1].unwrap_or(defaults.relativenumber),
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum Change {
    Set(bool),
    Toggle,
    Query,
}

#[derive(Debug, Clone)]
pub(crate) struct LineNumberCommand {
    pub local: bool,
    changes: Vec<(usize, Change)>,
}

impl LineNumberCommand {
    pub fn parse(text: &str) -> Option<Result<Self, String>> {
        let mut words = text.trim().trim_start_matches(':').split_whitespace();
        let local = match words.next()? {
            "set" | "se" => false,
            "setlocal" | "setl" => true,
            _ => return None,
        };
        Some((|| {
            let mut changes = Vec::new();
            for word in words {
                if changes.len() == 32 {
                    return Err("Too many editor options".into());
                }
                let (word, suffix) = if let Some(word) = word.strip_suffix('!') {
                    (word, Some(Change::Toggle))
                } else if let Some(word) = word.strip_suffix('?') {
                    (word, Some(Change::Query))
                } else {
                    (word, None)
                };
                let (name, value) = word
                    .strip_prefix("no")
                    .map_or((word, true), |name| (name, false));
                let index = match name {
                    "number" | "nu" => 0,
                    "relativenumber" | "rnu" => 1,
                    _ => return Err(format!("Unknown editor option: {word}")),
                };
                if !value && suffix.is_some() {
                    return Err(format!("Invalid editor option: {word}"));
                }
                changes.push((index, suffix.unwrap_or(Change::Set(value))));
            }
            if changes.is_empty() {
                changes.extend([(0, Change::Query), (1, Change::Query)]);
            }
            Ok(Self { local, changes })
        })())
    }

    pub fn apply(&self, current: LineNumberOptions) -> (LineNumberOptions, [bool; 2], Vec<String>) {
        let mut values = [current.number, current.relativenumber];
        let mut changed = [false; 2];
        let mut reports = Vec::new();
        for &(index, change) in &self.changes {
            match change {
                Change::Set(value) => {
                    values[index] = value;
                    changed[index] = true;
                }
                Change::Toggle => {
                    values[index] = !values[index];
                    changed[index] = true;
                }
                Change::Query => reports.push(format!(
                    "{}{}",
                    if values[index] { "" } else { "no" },
                    ["number", "relativenumber"][index]
                )),
            }
        }
        (
            LineNumberOptions {
                number: values[0],
                relativenumber: values[1],
            },
            changed,
            reports,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vim_line_number_modes_and_commands() {
        let absolute = LineNumberOptions::default();
        let hybrid = LineNumberOptions {
            relativenumber: true,
            ..absolute
        };
        let relative = LineNumberOptions {
            number: false,
            ..hybrid
        };
        for (options, expected) in [
            (absolute, [Some(1), Some(3), Some(5)]),
            (hybrid, [Some(2), Some(3), Some(2)]),
            (relative, [Some(2), Some(0), Some(2)]),
            (
                LineNumberOptions {
                    number: false,
                    relativenumber: false,
                },
                [None; 3],
            ),
        ] {
            assert_eq!([0, 2, 4].map(|line| options.label(line, 2, true)), expected);
            assert_eq!(options.label(2, 2, false), None);
        }
        assert_eq!(hybrid.label(8, 2, true), Some(6));
        assert_eq!(hybrid.label(8, 7, true), Some(1));
        let parse = |text| LineNumberCommand::parse(text).unwrap().unwrap();
        assert!(parse(":setlocal nonu rnu").local);
        assert_eq!(parse("setlocal nonu rnu").apply(absolute).0, relative);
        assert_eq!(
            parse("set number relativenumber!").apply(absolute).0,
            hybrid
        );
        assert_eq!(parse("set nornu").apply(hybrid).0, absolute);
        assert_eq!(
            parse("set nu? rnu?").apply(relative).2,
            ["nonumber", "relativenumber"]
        );
        assert_eq!(parse("set").apply(absolute).1, [false, false]);
        assert!(LineNumberCommand::parse("set nu unknown").unwrap().is_err());
        assert!(LineNumberCommand::parse("set nonu!").unwrap().is_err());
        assert!(LineNumberCommand::parse("select 1").is_none());
        let local = LineNumberOverrides([Some(false), None]);
        assert_eq!(local.resolve(hybrid), relative);
        assert_eq!(LineNumberOverrides::default().resolve(hybrid), hybrid);
    }
}
