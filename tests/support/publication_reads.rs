use super::{ForgeError, Read, crate_read, image_read, parse_formula};

#[test]
fn crate_reads_distinguish_publication_absence_and_unreadable_responses() {
    let version = "1.4.0".parse().unwrap();
    let body = r#"{"version":{"num":"1.4.0","yanked":false,"checksum":"checksum"}}"#;
    assert_eq!(
        crate_read(200, body, &version),
        Ok(Read::Present("checksum".into()))
    );
    assert_eq!(
        crate_read(200, &body.replace("false", "true"), &version),
        Ok(Read::Yanked)
    );
    assert_eq!(
        crate_read(
            404,
            r#"{"errors":[{"detail":"crate does not have a version"}]}"#,
            &version
        ),
        Ok(Read::Missing)
    );
    for status in [401, 403, 429, 500, 503] {
        assert_eq!(
            crate_read(status, body, &version),
            Err(ForgeError::Http(status))
        );
    }
    for body in ["not json", "{}", &body.replace("1.4.0", "1.3.0")] {
        assert_eq!(crate_read(200, body, &version), Err(ForgeError::Json));
    }
    assert_eq!(crate_read(404, "not json", &version), Err(ForgeError::Json));
}

#[test]
fn image_reads_require_the_digest_and_classify_only_manifest_404_as_missing() {
    assert_eq!(
        image_read(
            200,
            "HTTP/2 200\r\nDocker-Content-Digest: sha256:digest\r\n"
        ),
        Ok(Read::Present("sha256:digest".into()))
    );
    assert_eq!(image_read(404, ""), Ok(Read::Missing));
    assert_eq!(image_read(200, "Content-Length: 0"), Err(ForgeError::Json));
    for status in [401, 403, 429, 500, 503] {
        assert_eq!(image_read(status, ""), Err(ForgeError::Http(status)));
    }
}

#[test]
fn formula_reads_check_every_url_checksum_and_explicit_version() {
    let repo = "acme/widget".parse().unwrap();
    let tag = "widget-1.4.0".parse().unwrap();
    let version = "1.4.0".parse().unwrap();
    let good = format!(
        "  url \"https://github.com/acme/widget/releases/download/widget-1.4.0/widget.tar.gz\"\n  sha256 \"{}\"\n",
        "a".repeat(64)
    );
    let read = |body: &str| parse_formula(body, "blob-sha", &repo, &tag, &version);
    assert_eq!(read(&good), Ok(Read::Present("blob-sha".into())));
    assert_eq!(
        read(&format!("version \"1.4.0\"\n{good}{good}")),
        Ok(Read::Present("blob-sha".into()))
    );
    assert_eq!(
        read(&good.replace("widget.tar.gz\"", "widget.tar.gz\", using: :curl")),
        Ok(Read::Present("blob-sha".into()))
    );
    let stale = good.replace("widget-1.4.0", "widget-1.3.0");
    assert_eq!(
        read(&format!("{good}{stale}")),
        Ok(Read::Stale("widget-1.3.0".into()))
    );
    assert_eq!(
        read(&format!("version \"1.3.0\"\n{good}")),
        Ok(Read::Stale("1.3.0".into()))
    );
    for bad in [
        String::new(),
        good.replace("acme/widget", "other/widget"),
        good.replace(&"a".repeat(64), "abc"),
        good.replace(&"a".repeat(64), &"A".repeat(64)),
        good.lines().next().unwrap().into(),
    ] {
        assert_eq!(read(&bad), Err(ForgeError::Json), "{bad}");
    }
}
