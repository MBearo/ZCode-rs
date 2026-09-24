use super::*;

#[test]
fn listings_and_manifests_normalize_like_node() {
    let entry = json!({"name":"p","displayName":"P","displayName_i18n":{"zh":"中","x":1},
        "icon":" ","author":{"name":"A","url":"u"},"examplePrompts":["go"," "],
        "examplePrompts_i18n":{"zh":["试",2],"en":[]},"requiresPaidPlan":"true"});
    assert_eq!(
        listing(&entry),
        Some(
            json!({"displayName":"P","displayNameI18n":{"zh":"中"},"author":"A","authorUrl":"u",
            "examplePrompts":["go"],"examplePromptsI18n":{"zh":["试"]}})
        )
    );
    assert_eq!(listing(&json!({"name":"p"})), None);
    let manifest = parse_manifest(&json!({"name":" market ","featured":["a"," "],
        "plugins":{"a":{"version":"1"},"b":{"name":"renamed"}}}))
    .unwrap();
    let names: Vec<_> = manifest.plugins.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, vec!["a", "renamed"], "an entry's own name wins");
    assert_eq!(manifest.featured, vec!["a"]);
    assert!(parse_manifest(&json!({"name":"Bad Name","plugins":[]})).is_none());
    assert_eq!(
        identity_pin(Some(
            &json!({"source":"url","type":"zip","url":"u","sha256":"z","sha":"s"})
        )),
        Some("z".into())
    );
    assert_eq!(identity_pin(Some(&json!({"commit":"c"}))), Some("c".into()));
    assert_eq!(
        default_source("owner/repo#v1"),
        json!({"source":"github","repo":"owner/repo","ref":"v1"})
    );
    assert_eq!(
        default_source("https://x/m.json"),
        json!({"source":"url","url":"https://x/m.json"})
    );
}

#[test]
fn declared_marketplaces_overlay_known_records() {
    let known = vec![
        json!({"id":"zcode-plugins-official","source":{"source":"url","url":"o"},"name":"o","pluginCount":1}),
        json!({"id":"team","source":{"source":"git","url":"a"},"name":"team","pluginCount":2}),
    ];
    let config = json!({"plugins":{"extraKnownMarketplaces":{
        "zcode-plugins-official":{"source":{"source":"url","url":"other"}},
        "team":{"source":{"source":"git","url":"b"}},
        "local":{"source":{"source":"directory","path":"m"}}}}});
    let records = effective(&config, Path::new("/u/.zcode/cli/config.json"), &known);
    let summary: Vec<_> = records
        .iter()
        .map(|(r, cached)| (r["id"].clone(), r["pluginCount"].clone(), *cached))
        .collect();
    assert_eq!(
        summary,
        vec![
            (json!("zcode-plugins-official"), json!(1), true),
            (json!("team"), json!(0), false),
            (json!("local"), json!(0), false),
        ]
    );
    assert_eq!(records[2].0["source"]["path"], "/u/.zcode/cli/m");
    let diagnostics = declaration_diagnostics(&config, Path::new("/u/c.json"), &known);
    assert_eq!(
        diagnostics[0].code,
        "plugin_marketplace_declaration_reserved"
    );
    assert_eq!(diagnostics[0].severity, Severity::Warning);
}
