#[path = "support/campaign.rs"]
mod campaign_fixture;
#[path = "support/idea.rs"]
mod idea_fixture;

use campaign_fixture::Campaign;
use idea_fixture::{append_expected, ideas, rules};
use serde_json::Value;

fn ok(result: Value) -> Value {
    assert_eq!(result["ok"], true, "{result}");
    result
}

fn date(result: &Value) -> &str {
    &result["at"].as_str().unwrap()[..10]
}

fn assert_journal(c: &Campaign, before: &str, result: &Value, title: &str, body: &str) {
    let at = result["at"].as_str().unwrap();
    assert_eq!(result["journal"], cm::journal::heading(at, title));
    let mut expected = before.to_owned();
    while !expected.ends_with("\n\n") {
        expected.push('\n');
    }
    expected.push_str(&cm::journal::entry(at, title, Some(body)));
    assert_eq!(c.journal_text(), expected);
}

#[test]
fn admit_appends_in_order_once_and_preserves_unowned_bytes() {
    let c = ideas();
    c.add_idea("IDEA-9003");
    let before = c.add_idea("IDEA-9001");
    ok(c.cm(&["idea", "admit", "IDEA-9003"]));
    let epoch = c.epoch_text();
    let journal = c.journal_text();
    let result = ok(c.cm(&["idea", "admit", "IDEA-9001"]));
    assert_eq!(result["command"], "idea");
    assert_eq!(result["id"], "IDEA-9001");
    assert_eq!(result["action"], "admit");
    assert_eq!(result["changed"], true);
    assert_eq!(
        c.epoch_text(),
        epoch.replace("ideas: [IDEA-9003]", "ideas: [IDEA-9003, IDEA-9001]")
    );
    let expected = before.replacen("---\n\n#", "current_epoch: epoch-alpha\n---\n\n#", 1);
    assert_eq!(
        c.read_idea("IDEA-9001"),
        append_expected(
            &expected,
            &format!("- {}: admitted to `epoch-alpha` (PLANNING).", date(&result))
        )
    );
    assert_journal(
        &c,
        &journal,
        &result,
        "idea admit IDEA-9001",
        "- idea: IDEA-9001\n- epoch: epoch-alpha\n",
    );
    let snapshot = c.idea_snapshot();
    assert_eq!(ok(c.cm(&["idea", "admit", "IDEA-9001"]))["changed"], false);
    assert_eq!(c.idea_snapshot(), snapshot);
}

#[test]
fn admission_recovers_each_interrupted_write_boundary() {
    for keep_epoch in [false, true] {
        let c = ideas();
        c.add_idea("IDEA-9001");
        let epoch = c.epoch_text();
        let journal = c.journal_text();
        ok(c.cm(&["idea", "admit", "IDEA-9001"]));
        let idea = c.read_idea("IDEA-9001");
        let finished_epoch = c.epoch_text();
        if !keep_epoch {
            c.write_epoch(&epoch);
        }
        c.restore_journal(&journal);
        let result = ok(c.cm(&["idea", "admit", "IDEA-9001"]));
        assert_eq!(result["changed"], true);
        assert_eq!(c.read_idea("IDEA-9001"), idea);
        assert_eq!(c.epoch_text(), finished_epoch);
        assert_journal(
            &c,
            &journal,
            &result,
            "idea admit IDEA-9001",
            "- idea: IDEA-9001\n- epoch: epoch-alpha\n",
        );
        let snapshot = c.idea_snapshot();
        assert_eq!(ok(c.cm(&["idea", "admit", "IDEA-9001"]))["changed"], false);
        assert_eq!(c.idea_snapshot(), snapshot);
    }
}

#[test]
fn move_changes_only_maturity_and_appends_one_history_line() {
    for path in [vec!["exploring", "designed"], vec!["designed"]] {
        let c = ideas();
        c.add_idea("IDEA-9001");
        ok(c.cm(&["idea", "admit", "IDEA-9001"]));
        let mut from = "captured";
        for to in path {
            let before = c.read_idea("IDEA-9001");
            let epoch = c.epoch_text();
            let journal = c.journal_text();
            let result = ok(c.cm(&["idea", "move", "IDEA-9001", to]));
            assert_eq!(result["maturity"], to);
            let expected =
                before.replace(&format!("maturity: {from}\n"), &format!("maturity: {to}\n"));
            assert_eq!(
                c.read_idea("IDEA-9001"),
                append_expected(
                    &expected,
                    &format!(
                        "- {}: maturity {from} → {to} in `epoch-alpha`.",
                        date(&result)
                    )
                )
            );
            assert_eq!(c.epoch_text(), epoch);
            assert_journal(
                &c,
                &journal,
                &result,
                "idea move IDEA-9001",
                &format!("- idea: IDEA-9001\n- epoch: epoch-alpha\n- maturity: {from} → {to}\n"),
            );
            from = to;
        }
        for to in ["captured", "designed"] {
            let snapshot = c.idea_snapshot();
            assert_eq!(
                rules(&c.cm(&["idea", "move", "IDEA-9001", to])),
                ["idea-maturity"]
            );
            assert_eq!(c.idea_snapshot(), snapshot);
        }
    }
}

#[test]
fn all_five_outcomes_preserve_membership_and_idea_bodies() {
    for (word, flags, details) in [
        (
            "landed",
            vec!["--note", "tool-x #12 -> v1.2.0"],
            "landed; tool-x #12 -> v1.2.0",
        ),
        (
            "partial",
            vec!["--to", "epoch-beta", "--note", "tool-x #12"],
            "partial, remainder to `epoch-beta`; tool-x #12",
        ),
        (
            "deferred",
            vec!["--to", "unassigned"],
            "deferred to unassigned",
        ),
        (
            "superseded",
            vec!["--by", "IDEA-9004"],
            "superseded by IDEA-9004",
        ),
        (
            "cancelled",
            vec!["--note", "the upstream API was withdrawn"],
            "cancelled; the upstream API was withdrawn",
        ),
    ] {
        let c = ideas();
        c.add_idea("IDEA-9001");
        c.add_idea("IDEA-9004");
        ok(c.cm(&["idea", "admit", "IDEA-9001"]));
        let before = c.read_idea("IDEA-9001");
        let replacement = c.read_idea("IDEA-9004");
        let epoch = c.epoch_text();
        let journal = c.journal_text();
        let mut args = vec!["idea", "outcome", "IDEA-9001", word];
        args.extend(flags);
        let result = ok(c.cm(&args));
        assert_eq!(result["outcome"], word);
        assert_eq!(result["action"], "outcome");
        assert_eq!(
            c.read_idea("IDEA-9001"),
            append_expected(
                &before,
                &format!("- {}: outcome in `epoch-alpha`: {details}.", date(&result))
            )
        );
        assert_eq!(c.read_idea("IDEA-9004"), replacement);
        assert_eq!(c.epoch_text(), epoch);
        assert_journal(
            &c,
            &journal,
            &result,
            "idea outcome IDEA-9001",
            &format!("- idea: IDEA-9001\n- epoch: epoch-alpha\n- outcome: {details}\n"),
        );
        let snapshot = c.idea_snapshot();
        assert_eq!(rules(&c.cm(&args)), ["idea-outcome-twice"]);
        assert_eq!(c.idea_snapshot(), snapshot);
        if matches!(word, "landed" | "superseded" | "cancelled") {
            assert_eq!(
                rules(&c.cm(&["idea", "admit", "IDEA-9001"])),
                ["idea-terminal"]
            );
        }
    }
}

#[test]
fn nonterminal_outcomes_allow_admission_in_a_later_epoch() {
    for word in ["partial", "deferred"] {
        let c = ideas();
        c.add_idea("IDEA-9001");
        ok(c.cm(&["idea", "admit", "IDEA-9001"]));
        let mut args = vec!["idea", "outcome", "IDEA-9001", word, "--to", "epoch-beta"];
        if word == "partial" {
            args.extend(["--note", "one part landed"]);
        }
        ok(c.cm(&args));
        c.edit_epoch("epoch: epoch-alpha", "epoch: epoch-beta");
        c.edit_epoch("ideas: [IDEA-9001]\n", "");
        let before = c.read_idea("IDEA-9001");
        let result = ok(c.cm(&["idea", "admit", "IDEA-9001"]));
        let expected = before.replace("current_epoch: epoch-alpha", "current_epoch: epoch-beta");
        assert_eq!(
            c.read_idea("IDEA-9001"),
            append_expected(
                &expected,
                &format!("- {}: admitted to `epoch-beta` (PLANNING).", date(&result))
            )
        );
        ok(c.cm(&[
            "idea",
            "outcome",
            "IDEA-9001",
            "deferred",
            "--to",
            "unassigned",
        ]));
    }
}

#[test]
fn move_and_outcome_need_this_epoch_and_admission_refuses_open_elsewhere() {
    let c = ideas();
    c.add_idea("IDEA-9001");
    c.edit_idea(
        "IDEA-9001",
        "maturity: captured",
        "maturity: captured\ncurrent_epoch: epoch-beta",
    );
    for (args, rule) in [
        (
            vec!["idea", "move", "IDEA-9001", "exploring"],
            "idea-not-here",
        ),
        (
            vec![
                "idea",
                "outcome",
                "IDEA-9001",
                "deferred",
                "--to",
                "unassigned",
            ],
            "idea-not-here",
        ),
        (vec!["idea", "admit", "IDEA-9001"], "idea-open-elsewhere"),
    ] {
        let snapshot = c.idea_snapshot();
        assert_eq!(rules(&c.cm(&args)), [rule]);
        assert_eq!(c.idea_snapshot(), snapshot);
    }
}

#[test]
fn prose_continuations_nested_bullets_and_fences_never_count_as_outcomes() {
    for history in [
        "- 2026-01-02: we considered an outcome in `epoch-beta`: deferred to unassigned.",
        "- 2026-01-02T09:30:00-05:00: free prose.\n  - 2026-01-03: outcome in `epoch-beta`: deferred to unassigned.",
        "```md\n- 2026-01-02: outcome in `epoch-beta`: deferred to unassigned.\n```",
        "- not-a-date: outcome in `epoch-beta`: deferred to unassigned.",
    ] {
        let c = ideas();
        c.add_idea("IDEA-9001");
        c.edit_idea(
            "IDEA-9001",
            "maturity: captured",
            "maturity: captured\ncurrent_epoch: epoch-beta",
        );
        c.history("IDEA-9001", history);
        let snapshot = c.idea_snapshot();
        assert_eq!(
            rules(&c.cm(&["idea", "admit", "IDEA-9001"])),
            ["idea-open-elsewhere"]
        );
        assert_eq!(c.idea_snapshot(), snapshot);
    }
}

#[test]
fn date_and_full_timestamp_outcomes_are_state_only_in_epoch_history() {
    for stamp in ["2026-01-02", "2026-01-02T09:30:00-05:00"] {
        let c = ideas();
        c.add_idea("IDEA-9001");
        c.edit_idea(
            "IDEA-9001",
            "maturity: captured",
            "maturity: captured\ncurrent_epoch: epoch-beta",
        );
        c.history("IDEA-9001", &format!("- {stamp}: outcome in `epoch-beta`: deferred to unassigned.\n  Continuation text.\n  - Nested text."));
        let before = c.read_idea("IDEA-9001");
        let result = ok(c.cm(&["idea", "admit", "IDEA-9001"]));
        let expected = before.replace("current_epoch: epoch-beta", "current_epoch: epoch-alpha");
        assert_eq!(
            c.read_idea("IDEA-9001"),
            append_expected(
                &expected,
                &format!("- {}: admitted to `epoch-alpha` (PLANNING).", date(&result))
            )
        );
    }
    let c = ideas();
    c.add_idea("IDEA-9001");
    c.edit_idea(
        "IDEA-9001",
        "maturity: captured",
        "maturity: captured\ncurrent_epoch: epoch-beta",
    );
    c.edit_idea(
        "IDEA-9001",
        "Keep the final section too.",
        "- 2026-01-02: outcome in `epoch-beta`: deferred to unassigned.",
    );
    assert_eq!(
        rules(&c.cm(&["idea", "admit", "IDEA-9001"])),
        ["idea-open-elsewhere"]
    );
}

#[test]
fn unknown_and_ambiguous_ids_refuse_without_writing() {
    let c = ideas();
    c.add_idea("IDEA-9001");
    for args in [
        vec!["idea", "admit", "IDEA-9999"],
        vec!["idea", "move", "IDEA-9999", "exploring"],
        vec![
            "idea",
            "outcome",
            "IDEA-9999",
            "deferred",
            "--to",
            "unassigned",
        ],
    ] {
        let snapshot = c.idea_snapshot();
        assert_eq!(rules(&c.cm(&args)), ["idea-unknown"]);
        assert_eq!(c.idea_snapshot(), snapshot);
    }
    ok(c.cm(&["idea", "admit", "IDEA-9001"]));
    assert_eq!(
        rules(&c.cm(&[
            "idea",
            "outcome",
            "IDEA-9001",
            "superseded",
            "--by",
            "IDEA-9999"
        ])),
        ["idea-unknown"]
    );
    c.duplicate_idea("IDEA-9001");
    for args in [vec!["idea", "admit", "IDEA-9001"], vec!["lint"]] {
        let snapshot = c.idea_snapshot();
        let report = c.cm(&args);
        let duplicates: Vec<_> = report["violations"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|v| v["rule"] == "idea-duplicate-id")
            .collect();
        assert_eq!(duplicates.len(), 2, "{report}");
        assert_ne!(duplicates[0]["path"], duplicates[1]["path"]);
        for violation in &duplicates {
            let other = duplicates
                .iter()
                .find(|other| other["path"] != violation["path"])
                .unwrap();
            assert_eq!(
                violation["message"],
                format!(
                    "IDEA-9001 also appears in {}",
                    other["path"].as_str().unwrap()
                )
            );
        }
        assert_eq!(c.idea_snapshot(), snapshot);
    }
}

#[test]
fn invalid_outcome_flags_and_multiline_notes_refuse_without_writes() {
    let c = ideas();
    c.add_idea("IDEA-9001");
    ok(c.cm(&["idea", "admit", "IDEA-9001"]));
    for flags in [
        vec!["landed"],
        vec!["partial", "--note", "part"],
        vec!["partial", "--to", "epoch-beta"],
        vec!["deferred"],
        vec!["superseded"],
        vec!["cancelled"],
        vec!["landed", "--note", ""],
        vec!["cancelled", "--note", "a\nb"],
        vec!["cancelled", "--note", "a\rb"],
        vec!["deferred", "--to", "bad`target"],
        vec!["landed", "--note", "evidence", "--to", "epoch-beta"],
    ] {
        let mut args = vec!["idea", "outcome", "IDEA-9001"];
        args.extend(flags);
        let snapshot = c.idea_snapshot();
        assert_eq!(rules(&c.cm(&args)), ["idea-outcome-flags"]);
        assert_eq!(c.idea_snapshot(), snapshot);
    }
}

#[test]
fn missing_directory_block_list_and_missing_history_have_named_refusals() {
    for (epoch_from, epoch_to, idea_from, idea_to, rule) in [
        ("ideas_dir: ../ideas\n", "", "", "", "idea-dir"),
        (
            "ideas_dir: ../ideas",
            "ideas_dir: ../ideas\nideas:\n  - IDEA-9001",
            "",
            "",
            "idea-list-form",
        ),
        ("", "", "## Epoch history", "## Notes", "idea-history"),
    ] {
        let c = ideas();
        c.add_idea("IDEA-9001");
        if !epoch_from.is_empty() {
            c.edit_epoch(epoch_from, epoch_to);
        }
        if !idea_from.is_empty() {
            c.edit_idea("IDEA-9001", idea_from, idea_to);
        }
        let snapshot = c.idea_snapshot();
        assert_eq!(rules(&c.cm(&["idea", "admit", "IDEA-9001"])), [rule]);
        assert_eq!(c.idea_snapshot(), snapshot);
    }
}

#[test]
fn closed_campaign_and_closed_epoch_refuse_admission() {
    for seal_campaign in [false, true] {
        let c = ideas();
        c.add_idea("IDEA-9001");
        let rule = if seal_campaign {
            ok(c.cm(&["close"]));
            "campaign-closed"
        } else {
            c.edit_epoch("status: PLANNING", "status: CLOSED");
            "epoch-closed"
        };
        let snapshot = c.idea_snapshot();
        assert_eq!(rules(&c.cm(&["idea", "admit", "IDEA-9001"])), [rule]);
        assert_eq!(c.idea_snapshot(), snapshot);
    }
}

#[test]
fn lint_checks_required_keys_maturity_and_filename() {
    for (from, to, expected) in [
        ("id: IDEA-9001\n", "", vec!["idea-front-matter"]),
        ("maturity: captured\n", "", vec!["idea-front-matter"]),
        (
            "maturity: captured",
            "maturity: shipped",
            vec!["idea-maturity"],
        ),
        ("id: IDEA-9001", "id: IDEA-9002", vec!["idea-filename"]),
    ] {
        let c = ideas();
        c.add_idea("IDEA-9001");
        c.edit_idea("IDEA-9001", from, to);
        assert_eq!(rules(&c.cm(&["lint"])), expected);
    }
}

#[test]
fn lint_checks_epoch_membership_in_both_directions_and_admit_repairs_it() {
    let c = ideas();
    c.add_idea("IDEA-9001");
    c.edit_idea(
        "IDEA-9001",
        "maturity: captured",
        "maturity: captured\ncurrent_epoch: epoch-alpha",
    );
    assert_eq!(rules(&c.cm(&["lint"])), ["idea-epoch-list"]);
    ok(c.cm(&["idea", "admit", "IDEA-9001"]));
    ok(c.cm(&["lint"]));
    c.edit_epoch("ideas: [IDEA-9001]", "ideas: [IDEA-9001, IDEA-9999]");
    assert_eq!(rules(&c.cm(&["lint"])), ["idea-epoch-list"]);
    c.edit_epoch("ideas: [IDEA-9001, IDEA-9999]", "ideas: [IDEA-9001]");
    c.edit_idea(
        "IDEA-9001",
        "current_epoch: epoch-alpha",
        "current_epoch: epoch-beta",
    );
    assert_eq!(rules(&c.cm(&["lint"])), ["idea-epoch-list"]);
    c.history(
        "IDEA-9001",
        "- 2026-01-02: outcome in `epoch-beta`: deferred to unassigned.",
    );
    ok(c.cm(&["idea", "admit", "IDEA-9001"]));
    ok(c.cm(&["lint"]));
}

#[test]
fn no_ideas_dir_leaves_existing_campaign_lint_unchanged() {
    let c = ideas();
    c.add_idea("IDEA-9001");
    c.duplicate_idea("IDEA-9001");
    c.edit_epoch("ideas_dir: ../ideas\n", "");
    ok(c.cm(&["lint"]));
}

#[test]
fn crlf_and_no_final_newline_are_preserved_outside_allowed_edits() {
    let c = ideas();
    let text = c.add_idea("IDEA-9001");
    c.write_idea(
        "IDEA-9001",
        text.replace('\n', "\r\n").trim_end_matches("\r\n"),
    );
    c.write_epoch(&c.epoch_text().replace('\n', "\r\n"));
    let before = c.read_idea("IDEA-9001");
    let epoch = c.epoch_text();
    let result = ok(c.cm(&["idea", "admit", "IDEA-9001"]));
    let expected = before.replacen(
        "---\r\n\r\n#",
        "current_epoch: epoch-alpha\r\n---\r\n\r\n#",
        1,
    );
    assert_eq!(
        c.read_idea("IDEA-9001"),
        append_expected(
            &expected,
            &format!("- {}: admitted to `epoch-alpha` (PLANNING).", date(&result))
        )
    );
    assert_eq!(
        c.epoch_text(),
        epoch.replacen("---\r\n\r\n#", "ideas: [IDEA-9001]\r\n---\r\n\r\n#", 1)
    );
    let before = c.read_idea("IDEA-9001");
    let result = ok(c.cm(&["idea", "move", "IDEA-9001", "designed"]));
    assert_eq!(
        c.read_idea("IDEA-9001"),
        append_expected(
            &before.replace("maturity: captured", "maturity: designed"),
            &format!(
                "- {}: maturity captured → designed in `epoch-alpha`.",
                date(&result)
            )
        )
    );
}

#[test]
fn unfinished_history_fences_refuse_instead_of_hiding_new_state() {
    let c = ideas();
    c.add_idea("IDEA-9001");
    c.history("IDEA-9001", "```md\nAn unfinished example.");
    let snapshot = c.idea_snapshot();
    assert_eq!(
        rules(&c.cm(&["idea", "admit", "IDEA-9001"])),
        ["idea-history"]
    );
    assert_eq!(c.idea_snapshot(), snapshot);
}
