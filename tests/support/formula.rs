pub fn unsupported_arguments(value: &str) -> Vec<(&'static str, String)> {
    vec![
        (
            "hex escape",
            format!("\"{}\"", value.replace("1.4.0", r"\x31.3.0")),
        ),
        ("backslash escape", format!(r#""{value}\\suffix""#)),
        ("newline escape", format!(r#""{value}\n""#)),
        (
            "braced interpolation",
            format!("\"{}\"", value.replace("1.4.0", "#{version}")),
        ),
        (
            "instance interpolation",
            format!("\"{}\"", value.replace("1.4.0", "#@x")),
        ),
        (
            "global interpolation",
            format!("\"{}\"", value.replace("1.4.0", "#$x")),
        ),
        ("hash after value", format!(r#""{value}#@x""#)),
        ("sub expression", format!(r#""{value}".sub("4", "3")"#)),
        (
            "call result expression",
            format!(r#"("{value}").sub("4", "3")"#),
        ),
        ("concatenation", format!(r#""{value}" + suffix"#)),
        (
            "download option expression",
            format!(r#""{value}", using: :curl.sub("curl", "git")"#),
        ),
        ("percent literal", format!("%q({value})")),
        ("single quotes", format!("'{value}'")),
        ("heredoc", format!("<<~LITERAL\n{value}\nLITERAL")),
        ("literal newline", format!("\"{value}\nmore\"")),
    ]
}
