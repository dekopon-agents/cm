use std::path::PathBuf;

pub struct Heading<'a> {
    pub line: usize,
    pub level: usize,
    pub text: &'a str,
}

pub fn first_difference(now: &str, then: &str) -> usize {
    now.lines()
        .zip(then.lines())
        .position(|(a, b)| a != b)
        .unwrap_or_else(|| now.lines().count().min(then.lines().count()))
        + 1
}

#[derive(Default)]
struct Fences {
    open: Option<(u8, usize)>,
}

impl Fences {
    fn contains(&mut self, line: &str) -> bool {
        let trimmed = line.trim_start_matches(' ');
        if line.len() - trimmed.len() > 3 {
            return self.open.is_some();
        }
        let bytes = trimmed.as_bytes();
        let Some(&character) = bytes.first() else {
            return self.open.is_some();
        };
        if character != b'`' && character != b'~' {
            return self.open.is_some();
        }
        let length = bytes.iter().take_while(|byte| **byte == character).count();
        let rest = &trimmed[length..];
        match self.open {
            Some((opener, minimum)) => {
                if character == opener
                    && length >= minimum
                    && rest.trim_matches([' ', '\t']).is_empty()
                {
                    self.open = None;
                }
                true
            }
            None => {
                if length >= 3 && (character != b'`' || !rest.contains('`')) {
                    self.open = Some((character, length));
                    true
                } else {
                    false
                }
            }
        }
    }
}

pub(crate) fn unterminated_fence(text: &str) -> bool {
    let mut fences = Fences::default();
    for line in text.lines() {
        fences.contains(line);
    }
    fences.open.is_some()
}

pub fn headings(text: &str) -> Vec<Heading<'_>> {
    let mut fences = Fences::default();
    let mut out = Vec::new();
    let mut paragraph = None;
    for (index, line) in text.lines().enumerate() {
        if fences.contains(line) {
            paragraph = None;
            continue;
        }
        let trimmed = line.trim_start_matches(' ');
        if line.len() - trimmed.len() > 3 {
            paragraph = None;
            continue;
        }
        let line = trimmed;
        let level = line.bytes().take_while(|byte| *byte == b'#').count();
        if (1..=6).contains(&level) && line[level..].starts_with(' ') {
            out.push(Heading {
                line: index + 1,
                level,
                text: line[level + 1..].trim(),
            });
            paragraph = None;
        } else if let Some(level) = setext_level(line) {
            if let Some((start, title)) = paragraph.take() {
                out.push(Heading {
                    line: start,
                    level,
                    text: title,
                });
            }
        } else if line.trim().is_empty() {
            paragraph = None;
        } else if paragraph.is_none() {
            paragraph = Some((index + 1, line.trim()));
        }
    }
    out
}

pub(crate) fn setext_level(line: &str) -> Option<usize> {
    let line = line.trim_end();
    if !line.is_empty() && line.bytes().all(|byte| byte == b'=') {
        Some(1)
    } else if !line.is_empty() && line.bytes().all(|byte| byte == b'-') {
        Some(2)
    } else {
        None
    }
}

/// Plain kickoff labels outside code fences, such as `Outcome:` and `Files:`.
pub(crate) fn labels(text: &str) -> Vec<(usize, &str)> {
    let mut fences = Fences::default();
    text.lines()
        .enumerate()
        .filter_map(|(index, line)| {
            if fences.contains(line) {
                return None;
            }
            let trimmed = line.trim_start_matches(' ');
            if line.len() - trimmed.len() > 3 {
                return None;
            }
            let (name, _) = trimmed.split_once(':')?;
            (name.starts_with(|c: char| c.is_ascii_alphabetic())
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_')))
            .then_some((index + 1, name))
        })
        .collect()
}

pub fn code_spans(text: &str) -> Vec<(usize, &str)> {
    let mut fences = Fences::default();
    let mut out = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if fences.contains(line) {
            continue;
        }
        let bytes = line.as_bytes();
        let mut at = 0;
        while at < bytes.len() {
            if bytes[at] != b'`' {
                at += 1;
                continue;
            }
            let run = bytes[at..].iter().take_while(|byte| **byte == b'`').count();
            let open_end = at + run;
            let mut search = open_end;
            let mut closed = None;
            while search < bytes.len() {
                if bytes[search] == b'`' {
                    let close = bytes[search..]
                        .iter()
                        .take_while(|byte| **byte == b'`')
                        .count();
                    if close == run {
                        closed = Some(search);
                        break;
                    }
                    search += close;
                } else {
                    search += 1;
                }
            }
            match closed {
                Some(close) => {
                    out.push((index + 1, line[open_end..close].trim()));
                    at = close + run;
                }
                None => at = open_end,
            }
        }
    }
    out
}

pub fn named_path(span: &str, home: Option<&std::path::Path>) -> Option<PathBuf> {
    if span.is_empty()
        || span.contains(char::is_whitespace)
        || span.contains(['*', '?', '<', '>', '$', '{', '}', '[', ']', '|'])
    {
        return None;
    }
    let span = strip_location(span);
    if let Some(rest) = span.strip_prefix("~/") {
        return home.map(|home| home.join(rest));
    }
    let rest = span.strip_prefix('/')?;
    if rest.split('/').filter(|part| !part.is_empty()).count() < 2 {
        return None;
    }
    Some(PathBuf::from(span))
}

fn strip_location(span: &str) -> &str {
    let mut span = span;
    for _ in 0..2 {
        match span.rsplit_once(':') {
            Some((head, tail))
                if !tail.is_empty()
                    && tail.split('-').all(|part| {
                        !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit())
                    }) =>
            {
                span = head;
            }
            _ => break,
        }
    }
    span
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn spans_skip_fences_and_match_runs() {
        let text = "a `x` and ``y ` z`` end\n```\n`inside`\n```\n`unclosed";
        let spans = code_spans(text);
        assert_eq!(spans, vec![(1, "x"), (1, "y ` z")]);
    }

    #[test]
    fn paths_from_spans() {
        let home = Path::new("/home/u");
        assert_eq!(
            named_path("~/code/x", Some(home)),
            Some(PathBuf::from("/home/u/code/x"))
        );
        assert_eq!(
            named_path("/a/b.rs:286", None),
            Some(PathBuf::from("/a/b.rs"))
        );
        assert_eq!(
            named_path("/a/b.rs:10-20", None),
            Some(PathBuf::from("/a/b.rs"))
        );
        assert_eq!(named_path("/loop", None), None);
        assert_eq!(named_path("//!", None), None);
        assert_eq!(named_path("//", None), None);
        assert_eq!(named_path("/a/b c", None), None);
        assert_eq!(named_path("/a/<step>/x", None), None);
        assert_eq!(named_path("/a/turn-*.pid", None), None);
        assert_eq!(named_path("relative/path", None), None);
        assert_eq!(named_path("cargo", None), None);
    }

    #[test]
    fn headings_skip_fences() {
        let text = "# A\n```\n# not\n```\n   ## B\n\nC\n===\n\n  D\n  ---\n\n#nope\n";
        let found: Vec<_> = headings(text)
            .into_iter()
            .map(|heading| (heading.line, heading.level, heading.text))
            .collect();
        assert_eq!(
            found,
            vec![(1, 1, "A"), (5, 2, "B"), (7, 1, "C"), (10, 2, "D")]
        );
    }
}
