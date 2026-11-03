//! The `## Findings` section of a PR body.

use orchestral::findings::{self, Finding, Outcome, Problem};

fn body(section: &str) -> String {
    format!(
        "Closes #12\n\n## Summary\n\nAdds sign-up.\n\n## Findings\n\n{section}\n## Verification\n\nTests pass.\n"
    )
}

#[test]
fn none_is_a_valid_section() {
    assert_eq!(findings::check(&body("None\n")), Ok(Vec::new()));
}

#[test]
fn every_decided_outcome_is_valid() {
    let section = "- [issue #123] Avatar upload has no size limit\n\
                   - [fixed] Reviewer: off-by-one in pagination\n\
                   - [dropped: internal script, no user impact] Slow seed script\n";

    assert_eq!(
        findings::check(&body(section)),
        Ok(vec![
            Finding {
                outcome: Outcome::Issue(123),
                text: "Avatar upload has no size limit".into()
            },
            Finding {
                outcome: Outcome::Fixed,
                text: "Reviewer: off-by-one in pagination".into()
            },
            Finding {
                outcome: Outcome::Dropped("internal script, no user impact".into()),
                text: "Slow seed script".into()
            },
        ])
    );
}

#[test]
fn the_section_runs_to_the_end_of_the_body() {
    let text = "## Summary\n\nx\n\n## Findings\n- [fixed] A typo\r\n";

    assert_eq!(findings::check(text).unwrap().len(), 1);
}

#[test]
fn a_pending_finding_parses_but_fails_the_check() {
    let text = body("- [pending] Login allows an empty name (src/auth.ts)\n- [fixed] A typo\n");

    assert_eq!(findings::parse(&text).unwrap()[0].outcome, Outcome::Pending);
    assert_eq!(
        findings::check(&text),
        Err(Problem::Pending(vec![
            "Login allows an empty name (src/auth.ts)".into()
        ]))
    );
}

#[test]
fn an_unknown_outcome_fails() {
    for tag in [
        "done",
        "issue #",
        "issue #12a",
        "issue 12",
        "dropped:",
        "dropped: ",
        "Fixed",
    ] {
        let text = body(&format!("- [{tag}] Something\n"));
        assert_eq!(
            findings::check(&text),
            Err(Problem::Tag(tag.into())),
            "[{tag}]"
        );
    }
}

#[test]
fn a_tag_ends_at_the_first_bracket() {
    let text = body("- [dropped: see [docs]] Something\n");

    // So a reason can't hold a `]`: the rest becomes the text. A known
    // limit of the format, which the reviewer and summaries show as written.
    assert_eq!(
        findings::check(&text),
        Ok(vec![Finding {
            outcome: Outcome::Dropped("see [docs".into()),
            text: "] Something".into()
        }])
    );
}

#[test]
fn any_other_line_fails() {
    for line in [
        "Nothing found.",
        "* [fixed] A typo",
        "- [fixed]",
        "- fixed: A typo",
        "None, really",
        "None.",
    ] {
        assert_eq!(
            findings::check(&body(&format!("{line}\n"))),
            Err(Problem::Line(line.into())),
            "{line}"
        );
    }
    // `None` alongside a finding is another line too.
    assert_eq!(
        findings::check(&body("None\n- [fixed] A typo\n")),
        Err(Problem::Line("None".into()))
    );
}

#[test]
fn a_missing_or_empty_section_fails() {
    assert_eq!(
        findings::check("## Summary\n\nAdds sign-up.\n"),
        Err(Problem::Missing)
    );
    assert_eq!(
        findings::check("## Findings:\n\nNone\n"),
        Err(Problem::Missing)
    );
    assert_eq!(findings::check(&body("")), Err(Problem::Empty));
}
