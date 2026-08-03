use tracedecay_automation::skill_frontmatter::parse_skill_frontmatter;

#[test]
fn parses_native_skill_frontmatter() {
    let fields =
        parse_skill_frontmatter("---\nname: test-skill\ndescription: Use when testing.\n---\n")
            .unwrap();
    assert_eq!(fields["name"].as_scalar(), Some("test-skill"));
    assert_eq!(fields["description"].as_scalar(), Some("Use when testing."));
}
