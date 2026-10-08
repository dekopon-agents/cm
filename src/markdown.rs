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

fn is_fence(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("```") || trimmed.starts_with("~~~")
}

pub fn headings(text: &str) -> Vec<Heading<'_>> {
    let mut fenced = false;
    let mut out = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if is_fence(line) {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        let level = line.bytes().take_while(|byte| *byte == b'#').count();
        if (1..=6).contains(&level) && line[level..].starts_with(' ') {
            out.push(Heading {
                line: index + 1,
                level,
                text: line[level + 1..].trim(),
            });
        }
    }
    out
}

pub fn code_spans(text: &str) -> Vec<(usize, &str)> {
    let mut fenced = false;
    let mut out = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if is_fence(line) {
            fenced = !fenced;
            continue;
        }
        if fenced {
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
    if !rest.trim_end_matches('/').contains('/') {
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
        assert_eq!(named_path("/a/b c", None), None);
        assert_eq!(named_path("/a/<step>/x", None), None);
        assert_eq!(named_path("/a/turn-*.pid", None), None);
        assert_eq!(named_path("relative/path", None), None);
        assert_eq!(named_path("cargo", None), None);
    }

    #[test]
    fn headings_skip_fences() {
        let text = "# A\n```\n# not\n```\n## B\n#nope\n";
        let found: Vec<_> = headings(text)
            .into_iter()
            .map(|heading| (heading.line, heading.level, heading.text))
            .collect();
        assert_eq!(found, vec![(1, 1, "A"), (5, 2, "B")]);
    }
}
