use std::{
    env, fs,
    io::{self, Read, Write},
    path::Path,
    process::{exit, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Default)]
struct OrderedObject(Vec<(String, Json)>);
impl OrderedObject {
    fn get(&self, key: &str) -> Option<&Json> {
        self.0.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }
    fn get_mut(&mut self, key: &str) -> Option<&mut Json> {
        self.0.iter_mut().find(|(k, _)| k == key).map(|(_, v)| v)
    }
    fn contains_key(&self, key: &str) -> bool {
        self.get(key).is_some()
    }
    fn insert(&mut self, key: String, value: Json) {
        if let Some(old) = self.get_mut(&key) {
            *old = value;
        } else {
            self.0.push((key, value));
        }
    }
    fn remove(&mut self, key: &str) -> Option<Json> {
        let i = self.0.iter().position(|(k, _)| k == key)?;
        Some(self.0.remove(i).1)
    }
    fn pop_first(&mut self) -> Option<(String, Json)> {
        if self.0.is_empty() {
            None
        } else {
            Some(self.0.remove(0))
        }
    }
    fn clear(&mut self) {
        self.0.clear();
    }
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    fn iter(&self) -> impl Iterator<Item = &(String, Json)> {
        self.0.iter()
    }
    fn values_mut(&mut self) -> impl Iterator<Item = &mut Json> {
        self.0.iter_mut().map(|(_, v)| v)
    }
}
impl PartialEq for OrderedObject {
    fn eq(&self, other: &Self) -> bool {
        self.0.len() == other.0.len() && self.0.iter().all(|(k, v)| other.get(k) == Some(v))
    }
}
impl std::ops::Index<&str> for OrderedObject {
    type Output = Json;
    fn index(&self, key: &str) -> &Json {
        self.get(key).unwrap()
    }
}
impl IntoIterator for OrderedObject {
    type Item = (String, Json);
    type IntoIter = std::vec::IntoIter<Self::Item>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}
impl FromIterator<(String, Json)> for OrderedObject {
    fn from_iter<T: IntoIterator<Item = (String, Json)>>(items: T) -> Self {
        let mut out = Self::default();
        out.extend(items);
        out
    }
}
impl Extend<(String, Json)> for OrderedObject {
    fn extend<T: IntoIterator<Item = (String, Json)>>(&mut self, items: T) {
        for (k, v) in items {
            self.insert(k, v);
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Json {
    Null,
    Bool(bool),
    Number(String),
    String(String),
    Array(Vec<Json>),
    Object(OrderedObject),
}
impl Json {
    fn get(&self, key: &str) -> &Json {
        match self {
            Self::Object(m) => &m[key],
            _ => panic!("expected object"),
        }
    }
    fn get_mut(&mut self, key: &str) -> &mut Json {
        match self {
            Self::Object(m) => m.get_mut(key).unwrap(),
            _ => panic!("expected object"),
        }
    }
    fn obj(&mut self) -> &mut OrderedObject {
        match self {
            Self::Object(m) => m,
            _ => panic!("expected object"),
        }
    }
    fn string(&self) -> &str {
        match self {
            Self::String(s) => s,
            _ => panic!("expected string"),
        }
    }
    fn render(&self) -> String {
        match self {
            Self::Null => "null".into(),
            Self::Bool(b) => b.to_string(),
            Self::Number(n) => n.clone(),
            Self::String(s) => {
                let mut out = String::from("\"");
                for c in s.chars() {
                    match c {
                        '"' => out.push_str("\\\""),
                        '\\' => out.push_str("\\\\"),
                        '\n' => out.push_str("\\n"),
                        '\r' => out.push_str("\\r"),
                        '\t' => out.push_str("\\t"),
                        c if c < ' ' => out.push_str(&format!("\\u{:04x}", c as u32)),
                        c => out.push(c),
                    }
                }
                out.push('"');
                out
            }
            Self::Array(a) => format!(
                "[{}]",
                a.iter().map(Self::render).collect::<Vec<_>>().join(",")
            ),
            Self::Object(m) => format!(
                "{{{}}}",
                m.iter()
                    .map(|(k, v)| format!("{}:{}", Self::String(k.clone()).render(), v.render()))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
        }
    }
}
fn string(s: &str) -> Json {
    Json::String(s.into())
}
fn object(items: impl IntoIterator<Item = (String, Json)>) -> Json {
    Json::Object(items.into_iter().collect())
}
struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}
impl Parser<'_> {
    fn ws(&mut self) {
        while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }
    fn val(&mut self) -> Json {
        self.ws();
        match self.s[self.i] {
            b'"' => Json::String(self.str()),
            b'{' => {
                self.i += 1;
                let mut m = OrderedObject::default();
                self.ws();
                while self.s[self.i] != b'}' {
                    let k = self.str();
                    self.ws();
                    assert_eq!(self.s[self.i], b':');
                    self.i += 1;
                    m.insert(k, self.val());
                    self.ws();
                    if self.s[self.i] != b',' {
                        break;
                    }
                    self.i += 1;
                    self.ws();
                }
                self.i += 1;
                Json::Object(m)
            }
            b'[' => {
                self.i += 1;
                let mut a = Vec::new();
                self.ws();
                while self.s[self.i] != b']' {
                    a.push(self.val());
                    self.ws();
                    if self.s[self.i] != b',' {
                        break;
                    }
                    self.i += 1;
                }
                self.i += 1;
                Json::Array(a)
            }
            b't' => {
                self.i += 4;
                Json::Bool(true)
            }
            b'f' => {
                self.i += 5;
                Json::Bool(false)
            }
            b'n' => {
                self.i += 4;
                Json::Null
            }
            _ => {
                let start = self.i;
                while self.i < self.s.len() && b"-+0123456789.eE".contains(&self.s[self.i]) {
                    self.i += 1;
                }
                Json::Number(std::str::from_utf8(&self.s[start..self.i]).unwrap().into())
            }
        }
    }
    fn str(&mut self) -> String {
        assert_eq!(self.s[self.i], b'"');
        self.i += 1;
        let mut out = String::new();
        while self.s[self.i] != b'"' {
            if self.s[self.i] == b'\\' {
                self.i += 1;
                let c = self.s[self.i];
                self.i += 1;
                match c {
                    b'"' => out.push('"'),
                    b'\\' => out.push('\\'),
                    b'/' => out.push('/'),
                    b'b' => out.push('\u{8}'),
                    b'f' => out.push('\u{c}'),
                    b'n' => out.push('\n'),
                    b'r' => out.push('\r'),
                    b't' => out.push('\t'),
                    b'u' => {
                        let hi = self.hex();
                        let code = if (0xd800..=0xdbff).contains(&hi) {
                            assert_eq!(&self.s[self.i..self.i + 2], b"\\u");
                            self.i += 2;
                            0x10000 + ((hi - 0xd800) << 10) + (self.hex() - 0xdc00)
                        } else {
                            hi
                        };
                        out.push(char::from_u32(code).unwrap());
                    }
                    _ => panic!("invalid escape"),
                }
            } else {
                let s = std::str::from_utf8(&self.s[self.i..]).unwrap();
                let c = s.chars().next().unwrap();
                out.push(c);
                self.i += c.len_utf8();
            }
        }
        self.i += 1;
        out
    }
    fn hex(&mut self) -> u32 {
        let n = u32::from_str_radix(
            std::str::from_utf8(&self.s[self.i..self.i + 4]).unwrap(),
            16,
        )
        .unwrap();
        self.i += 4;
        n
    }
}
fn parse(s: &str) -> Json {
    let mut p = Parser {
        s: s.as_bytes(),
        i: 0,
    };
    let v = p.val();
    p.ws();
    assert_eq!(p.i, p.s.len());
    v
}
fn read(path: &Path) -> Json {
    parse(&fs::read_to_string(path).unwrap())
}
fn append(path: &Path, value: &Json) {
    writeln!(
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap(),
        "{}",
        value.render()
    )
    .unwrap();
}
fn context(prompt: &str) -> Json {
    parse(prompt.split_once("\n\nGate context:\n").unwrap().1)
}
fn acdd(exe: &str, root: &Path, args: &[&str]) -> Json {
    let mut cmd = Command::new(exe);
    cmd.arg("--root")
        .arg(root)
        .args(["--format", "json", "task"])
        .args(args)
        .current_dir(root);
    for key in [
        "ACDD_CONFIG",
        "ACDD_PROFILE",
        "ACDD_DB",
        "ACDD_WORKSPACE_ROOT",
        "ACDD_AGENT_SESSION_ID",
    ] {
        cmd.env_remove(key);
    }
    let output = cmd.output().unwrap();
    if !output.status.success() {
        #[cfg(unix)]
        let code = {
            use std::os::unix::process::ExitStatusExt;
            output.status.code().unwrap_or_else(|| -output.status.signal().unwrap_or(1))
        };
        #[cfg(not(unix))]
        let code = output.status.code().unwrap_or(1);
        eprintln!(
            "acdd {} FAILED (code {}):\nSTDOUT:\n{}\nSTDERR:\n{}",
            args.join(" "),
            code,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        exit(code);
    }
    parse(std::str::from_utf8(&output.stdout).unwrap())
}
fn task_role(exe: &str, session: &str, prompt: &str) {
    let claim = context(prompt);
    let root = Path::new(claim.get("worktree").string());
    let control = root.join(".acdd/lifecycle");
    let guidance = claim.get("workflow_guidance");
    let record = object([
        ("stage".into(), claim.get("stage").clone()),
        ("worker_id".into(), claim.get("worker_id").clone()),
        ("session".into(), string(session)),
        (
            "environment_session".into(),
            env::var("ACDD_AGENT_SESSION_ID").map_or(Json::Null, Json::String),
        ),
        ("claim_revision".into(), claim.get("claim_revision").clone()),
        (
            "rejection".into(),
            match guidance {
                Json::Object(m) => m.get("rejection").cloned().unwrap_or(Json::Null),
                _ => unreachable!(),
            },
        ),
        ("prompt".into(), string(prompt)),
    ]);
    append(&control.join("roles.jsonl"), &record);
    let mode = control.join("mode.json");
    if mode.exists() {
        match read(&mode) {
            Json::String(s) if s == "no-submit" => return,
            Json::String(s) if s == "exit-17" => exit(17),
            Json::String(s) if s == "sleep" => {
                thread::sleep(Duration::from_secs(60));
                return;
            }
            _ => {}
        }
    }
    let id = claim.get("task_id").string();
    let stage = claim.get("stage").string();
    match guidance.get("proof_kind").string() {
        "contract" | "command" | "seamtest" | "seam_test" => {
            let observation = acdd(exe, root, &["verify", id]);
            if let Json::Object(m) = observation {
                assert!(m.get("error").is_none_or(|e| *e == Json::Null));
            }
            acdd(
                exe,
                root,
                &["submit", id, "--stage", stage, "--action", "pass"],
            );
        }
        "review" => {
            let mut evidence = guidance.get("expected_evidence_template").clone();
            if stage == "review" && control.join("corrupt-template").exists() {
                evidence
                    .get_mut("proof")
                    .get_mut("review_proof")
                    .obj()
                    .remove("decision")
                    .expect("review decision");
            }
            if stage == "review" && control.join("wrong-wrapper").exists() {
                let proof = evidence.get_mut("proof").obj();
                let value = proof.pop_first().unwrap().1;
                assert!(proof.is_empty());
                proof.insert("wrong_proof".into(), value);
            }
            let rejection = control.join("reject.json");
            let mut action = "pass";
            let mut findings = String::new();
            if stage == "review" && rejection.exists() {
                findings = read(&rejection).render();
                fs::rename(rejection, control.join("rejected.json")).unwrap();
                let review = evidence.get_mut("proof").get_mut("review_proof");
                assert!(matches!(review.get("decision"), Json::String(_)));
                review.obj().insert("decision".into(), string("reject"));
                action = "reject";
            }
            let encoded = evidence.render();
            let mut args = vec![
                "submit",
                id,
                "--stage",
                stage,
                "--action",
                action,
                "--evidence",
                &encoded,
            ];
            if action == "reject" {
                args.extend(["--findings", &findings]);
            }
            acdd(exe, root, &args);
        }
        other => panic!("unexpected role proof kind: {other}"),
    }
}
fn materialize(value: Json, field: &str, placeholder: bool, root: &Path, decision: &str) -> Json {
    match value {
        Json::Object(m) => {
            let has_first = m.contains_key("first");
            let mut out = OrderedObject::default();
            for (k, v) in m {
                if field == "tasks" && k.starts_with('<') && k.ends_with('>') {
                    if !has_first && !root.join(".acdd/invalid-task-proof-map").exists() {
                        out.insert("first".into(), materialize(v, "", true, root, decision));
                    }
                } else if field == "tasks" {
                    out.insert(k, v);
                } else {
                    let item = materialize(v, &k, placeholder, root, decision);
                    out.insert(k, item);
                }
            }
            Json::Object(out)
        }
        Json::Array(a) => {
            let endpoint = if field == "incoming_seams" {
                "source_task"
            } else {
                "target_task"
            };
            let example = object([
                (endpoint.into(), string("<value>")),
                ("contract_type".into(), string("seam_dependency")),
                ("contract_ref".into(), string("<value>")),
                ("specification".into(), string("<value>")),
            ]);
            Json::Array(
                a.into_iter()
                    .filter(|v| {
                        !(placeholder
                            && (field == "incoming_seams" || field == "outgoing_seams")
                            && *v == example)
                    })
                    .map(|v| materialize(v, field, placeholder, root, decision))
                    .collect(),
            )
        }
        Json::String(s) if s.starts_with('<') && s.ends_with('>') => string(match field {
            "decision" => decision,
            "blackboard_decision_id" => "wave-blueprint",
            "layer" => "fixture seam",
            "summary" => "Exercise the pre-task proof boundary.",
            _ => "fixture value",
        }),
        other => other,
    }
}
fn wave_role(prompt: &str) {
    let context = context(prompt);
    let root = env::current_dir().unwrap();
    append(&root.join(".acdd/wave-roles.jsonl"), &context);
    if root.join(".acdd/no-wave-decision").exists() {
        return;
    }
    let gate = context.get("gate").string();
    let mut proof = context.get("expected_response").get("proof").clone();
    let mut decision = "pass";
    let mut findings = None;
    if gate == "blueprint_synthesis.review" && root.join(".acdd/stop-blueprint-review").exists() {
        exit(17);
    }
    if gate == "blueprint_synthesis.review" && !root.join(".acdd/blueprint-rejected").exists() {
        fs::write(root.join(".acdd/blueprint-rejected"), "").unwrap();
        decision = "reject";
        findings = Some(object([(
            "scope_findings".into(),
            Json::Array(vec![string("Revise the blueprint")]),
        )]));
    }
    if let Some(Json::Object(payload)) = proof.obj().values_mut().next() {
        if payload.contains_key("tasks") {
            if root.join(".acdd/corrupt-blueprint-missing").exists() {
                payload
                    .remove("blackboard_decision_id")
                    .expect("blueprint decision id");
            }
            if root.join(".acdd/corrupt-blueprint-tasks").exists() {
                payload.insert("tasks".into(), Json::Array(vec![]));
            }
            if root.join(".acdd/corrupt-blueprint-nested").exists() {
                payload.insert(
                    "tasks".into(),
                    object([(
                        "first".into(),
                        object([("layer".into(), Json::Number("7".into()))]),
                    )]),
                );
            }
        }
    }
    proof = materialize(proof, "", false, &root, decision);
    let override_path = root.join("root-proof.json");
    if override_path.exists() {
        let Json::Object(update) = read(&override_path) else {
            panic!("invalid root proof")
        };
        proof
            .obj()
            .values_mut()
            .next()
            .unwrap()
            .obj()
            .extend(update);
    }
    let mut response = object([
        ("gate".into(), context.get("gate").clone()),
        ("session_id".into(), context.get("session_id").clone()),
        ("decision".into(), string(decision)),
        ("proof".into(), proof),
    ]);
    if let Some(f) = findings {
        response.obj().insert("findings".into(), f);
    }
    if gate == "milestone_review" {
        if root.join(".acdd/stop-wave-review").exists() {
            exit(17);
        }
        let mut binding = context.get("subject_context").get("binding").clone();
        if root.join(".acdd/stale-wave-review").exists() {
            binding
                .obj()
                .insert("implementation_digest".into(), string("stale"));
        }
        response.obj().insert("binding".into(), binding);
        response.obj().insert("integration_review".into(),object([("worker_id".into(),context.get("session_id").clone()),("decision".into(),string("pass")),("summary".into(),string("Reviewed the supplied fixture authority, task receipts and implementation binding")),("scope_findings".into(),Json::Array(vec![])),("contract_findings".into(),Json::Array(vec![]))]));
    }
    println!("{}", response.render());
}
fn append_json_line(path: &Path, value: &Json) {
    append(path, value);
}

fn env_or_null(name: &str) -> Json {
    env::var(name).map_or(Json::Null, Json::String)
}

fn set_expected_field(payload: &mut OrderedObject, key: &str, value: Json) {
    let existing = payload
        .get(key)
        .unwrap_or_else(|| panic!("missing expected proof template key: {key}"));
    let same_type = matches!(
        (existing, &value),
        (Json::String(_), Json::String(_))
            | (Json::Bool(_), Json::Bool(_))
            | (Json::Number(_), Json::Number(_))
            | (Json::Array(_), Json::Array(_))
            | (Json::Object(_), Json::Object(_))
    );
    assert!(same_type, "invalid expected proof template type: {key}");
    payload.insert(key.into(), value);
}

fn workflow_runner(mode_flag: &str, prompt: &str) {
    let context = context(prompt);
    let gate = context.get("gate").string().to_owned();
    let session = env::var("ACDD_AGENT_SESSION_ID").unwrap();
    let mut record = vec![
        ("gate".into(), string(&gate)),
        ("goal".into(), context.get("goal").clone()),
        ("session".into(), string(&session)),
        ("findings".into(), context.get("findings").clone()),
        ("config".into(), env_or_null("ACDD_CONFIG")),
        ("profile".into(), env_or_null("ACDD_PROFILE")),
        ("db".into(), env_or_null("ACDD_DB")),
    ];
    if mode_flag == "ingress" {
        record.push(("context".into(), context.clone()));
        record.push(("prompt".into(), string(prompt)));
    }
    append_json_line(Path::new("workflow-role-sessions.jsonl"), &object(record));
    if Path::new("missing-decision").exists() {
        return;
    }
    let mut decision = "pass";
    let mut findings: Option<Json> = None;
    let mut proof = context.get("expected_response").get("proof").clone();
    let has_decision;
    {
        let payload = proof.obj().values_mut().next().unwrap().obj();
        if mode_flag == "ingress" && (gate == "specify" || gate == "design") {
            payload.clear();
        }
        if Path::new("root-proof.json").exists() {
            let Json::Object(update) = read(Path::new("root-proof.json")) else {
                panic!("invalid root proof");
            };
            payload.extend(update);
        }
        has_decision = payload.contains_key("decision");
        if has_decision {
            assert!(matches!(payload.get("decision"), Some(Json::String(_))));
            if let Some(summary) = payload.get("summary") {
                assert!(matches!(summary, Json::String(_)));
            }
            let reject_marker = if mode_flag == "per-gate" {
                format!("{gate}-rejected")
            } else {
                "review-rejected".to_owned()
            };
            if !Path::new(&reject_marker).exists() {
                fs::write(&reject_marker, "").unwrap();
                decision = "reject";
                findings = Some(object([(
                    "scope_findings".into(),
                    Json::Array(vec![string("Apply the required correction")]),
                )]));
            }
            payload.insert("decision".into(), string(decision));
            if payload.contains_key("summary") {
                payload.insert("summary".into(), string("Inspected this workflow run"));
            }
        }
    }
    let role_mode = fs::read_to_string("role-mode")
        .unwrap_or_default()
        .trim()
        .to_owned();
    if has_decision && decision == "reject" {
        let payload = proof.obj().values_mut().next().unwrap().obj();
        match role_mode.as_str() {
            "embedded" | "fenced-embedded" | "empty" | "conflict" => {
                payload.insert(
                    "step_findings".into(),
                    if role_mode == "empty" {
                        Json::Array(vec![])
                    } else {
                        Json::Array(vec![string("Repair this step")])
                    },
                );
                findings = if role_mode == "conflict" {
                    Some(object([(
                        "step_findings".into(),
                        Json::Array(vec![string("Different")]),
                    )]))
                } else {
                    None
                };
            }
            "nested-match" | "nested-conflict" | "nested-sibling-match"
            | "nested-sibling-conflict" => {
                payload.insert(
                    "findings".into(),
                    object([(
                        "step_findings".into(),
                        Json::Array(vec![string("Repair this step")]),
                    )]),
                );
                let text = if role_mode.ends_with("conflict") {
                    "Different"
                } else {
                    "Repair this step"
                };
                if role_mode.starts_with("nested-sibling") {
                    payload.insert(
                        "step_findings".into(),
                        Json::Array(vec![string(text)]),
                    );
                    findings = None;
                } else {
                    findings = Some(object([(
                        "step_findings".into(),
                        Json::Array(vec![string(text)]),
                    )]));
                }
            }
            "missing" => findings = None,
            _ => {}
        }
    }
    if has_decision && role_mode == "mismatch" {
        let payload = proof.obj().values_mut().next().unwrap().obj();
        payload.insert(
            "decision".into(),
            string(if decision == "pass" { "reject" } else { "pass" }),
        );
    }
    let response_session = if has_decision && role_mode == "identity" {
        "wrong".to_owned()
    } else {
        session
    };
    let response = if matches!(role_mode.as_str(), "array" | "fenced-array") {
        Json::Array(vec![
            string(&gate),
            string(&response_session),
            string(decision),
            proof,
            Json::Null,
            Json::Null,
            Json::Null,
        ])
    } else {
        object([
            ("gate".into(), string(&gate)),
            ("session_id".into(), string(&response_session)),
            ("decision".into(), string(decision)),
            ("proof".into(), proof),
            ("findings".into(), findings.unwrap_or(Json::Null)),
        ])
    };
    let mut text = response.render();
    if matches!(role_mode.as_str(), "duplicate" | "fenced-duplicate") {
        text = text.replacen(
            "\"decision\":\"pass\"",
            "\"decision\":\"reject\",\"decision\":\"pass\"",
            1,
        );
    }
    match role_mode.as_str() {
        "fenced" | "fenced-embedded" | "fenced-array" | "fenced-duplicate" => {
            text = format!("```json\n{text}\n```");
        }
        "incomplete" => text = format!("```json\n{text}"),
        "trailing" => text.push_str(" trailing prose"),
        "multiple" => {
            let second = text.clone();
            text.push('\n');
            text.push_str(&second);
        }
        _ => {}
    }
    println!("{text}");
}

fn polish_runner(prompt: &str) {
    let context = context(prompt);
    let gate = context.get("gate").string().to_owned();
    let session = env::var("ACDD_AGENT_SESSION_ID").unwrap();
    let mut proof = context.get("expected_response").get("proof").clone();
    {
        let payload = proof.obj().values_mut().next().unwrap().obj();
        set_expected_field(payload, "decision", string("pass"));
        set_expected_field(payload, "summary", string("Перевірено виправлення"));
    }
    append_json_line(
        Path::new("polish-sessions.jsonl"),
        &object([
            ("gate".into(), string(&gate)),
            ("session".into(), string(&session)),
        ]),
    );
    if gate == "polish" {
        fs::write("repaired", "").unwrap();
    }
    println!(
        "{}",
        object([
            ("gate".into(), string(&gate)),
            ("session_id".into(), string(&session)),
            ("decision".into(), string("pass")),
            ("proof".into(), proof),
        ])
        .render()
    );
}

fn author_runner(prompt: &str) {
    let context = context(prompt);
    let gate = context.get("gate").string().to_owned();
    let session = env::var("ACDD_AGENT_SESSION_ID").unwrap();
    let subject = context.get("subject").string();
    let kind = ["milestone", "plan", "workflow"]
        .into_iter()
        .find(|name| subject.contains(&format!("author-{name}")))
        .expect("author subject");
    let path_str = match kind {
        "milestone" => "docs/milestones/IN-001-pryklad.md",
        "plan" => "docs/plans/pryklad.md",
        "workflow" => "docs/workflows/pryklad.workflow.yaml",
        _ => unreachable!(),
    };
    let path = Path::new(path_str);
    let mut proof = context.get("expected_response").get("proof").clone();
    let mut decision = "pass";
    let mut findings = Json::Null;
    {
        let payload = proof.obj().values_mut().next().unwrap().obj();
        if gate == "synthesize" {
            let repaired = Path::new("rejected").exists();
            let fields: Vec<(&str, Json)> = match kind {
                "milestone" => vec![
                    ("milestone_id", string("IN-001-pryklad")),
                    ("manifest_path", string(path_str)),
                    ("wave_dag", Json::Array(vec![Json::Array(vec![string("first")])])),
                    ("scopes_disjoint", Json::Bool(true)),
                    ("protected_files_excluded", Json::Bool(true)),
                ],
                "plan" => vec![
                    ("plan_slug", string("pryklad")),
                    ("plan_path", string(path_str)),
                    ("step_count", Json::Number("2".into())),
                    (
                        "dependency_dag",
                        object([(
                            "Другий".into(),
                            Json::Array(vec![string("Перший")]),
                        )]),
                    ),
                    ("scopes_valid", Json::Bool(true)),
                ],
                "workflow" => vec![
                    ("workflow_name", string("pryklad")),
                    ("workflow_path", string(path_str)),
                    ("gate_count", Json::Number("1".into())),
                    ("transitions_valid", Json::Bool(true)),
                    ("taskless", Json::Bool(true)),
                ],
                _ => unreachable!(),
            };
            for (k, v) in fields {
                set_expected_field(payload, k, v);
            }
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let body = fs::read_to_string(if repaired {
                "author-final"
            } else {
                "author-draft"
            })
            .unwrap();
            fs::write(path, body).unwrap();
        } else {
            if !Path::new("rejected").exists() {
                decision = "reject";
                let field = match kind {
                    "milestone" => "scope_findings",
                    "plan" => "step_findings",
                    "workflow" => "structural_findings",
                    _ => unreachable!(),
                };
                let items = Json::Array(vec![string("Виправити чернетку")]);
                set_expected_field(payload, field, items.clone());
                findings = object([(field.into(), items)]);
            }
            set_expected_field(payload, "decision", string(decision));
            set_expected_field(payload, "summary", string("Перевірено документ"));
            if decision == "reject" {
                fs::write("rejected", "").unwrap();
            }
        }
    }
    append_json_line(
        Path::new("author-sessions.jsonl"),
        &object([
            ("gate".into(), string(&gate)),
            ("session".into(), string(&session)),
            ("decision".into(), string(decision)),
        ]),
    );
    println!(
        "{}",
        object([
            ("gate".into(), string(&gate)),
            ("session_id".into(), string(&session)),
            ("decision".into(), string(decision)),
            ("proof".into(), proof),
            ("findings".into(), findings),
        ])
        .render()
    );
}

fn artifact_runner(prompt: &str) {
    let context = context(prompt);
    let gate = context.get("gate").string().to_owned();
    let mut proof = context.get("expected_response").get("proof").clone();
    let mode = fs::read_to_string(".acdd/artifact-mode")
        .unwrap_or_default()
        .trim()
        .to_owned();
    let mut decision = "pass";
    {
        let payload = proof.obj().values_mut().next().unwrap().obj();
        if gate == "prerequisite" || gate == "z_prerequisite" {
            payload.insert("prerequisite".into(), string("prerequisite"));
            if mode == "bootstrap" && !Path::new("synthesized").exists() {
                payload.insert("artifact_path".into(), string("bootstrap"));
            }
            if mode == "earlier" {
                payload.insert("artifact_path".into(), string("earlier/path"));
            }
        } else if gate == "synthesize" || gate == "a_synthesize" {
            let new_path = if mode == "unsafe" {
                "../unsafe"
            } else {
                "new/path"
            };
            if Path::new("synthesized").exists() {
                payload.insert("plan_slug".into(), string("new-plan"));
                if mode == "bootstrap" {
                    decision = "reject";
                    fs::write("synth-rejected", "").unwrap();
                } else {
                    payload.insert("artifact_path".into(), string(new_path));
                }
            } else {
                fs::write("synthesized", "").unwrap();
                payload.insert("plan_slug".into(), string("old-plan"));
                payload.insert("artifact_path".into(), string("old/path"));
                payload.insert("removed".into(), string("stale"));
            }
            if mode == "reject-synth" {
                decision = "reject";
            }
        } else if gate == "review" {
            if !Path::new("reviewed").exists() {
                fs::write("reviewed", "").unwrap();
                decision = "reject";
            }
            payload.insert("decision".into(), string(decision));
        }
    }
    let findings = if decision == "reject" {
        object([("issue".into(), string("repair"))])
    } else {
        Json::Null
    };
    println!(
        "{}",
        object([
            ("gate".into(), string(&gate)),
            ("session_id".into(), string(&env::var("ACDD_AGENT_SESSION_ID").unwrap())),
            ("decision".into(), string(decision)),
            ("proof".into(), proof),
            ("findings".into(), findings),
        ])
        .render()
    );
}

fn plan_artifact_runner(exe: &str, prompt: &str) {
    let mut context = context(prompt);
    let taskless = matches!(&context, Json::Object(m) if m.contains_key("expected_response"));
    let (mut evidence, mut proof) = if taskless {
        (
            Json::Null,
            context.get("expected_response").get("proof").clone(),
        )
    } else {
        let ev = context
            .get("workflow_guidance")
            .get("expected_evidence_template")
            .clone();
        let p = ev.get("proof").clone();
        (ev, p)
    };
    {
        let payload = if taskless {
            proof.obj().values_mut().next().unwrap().obj()
        } else {
            evidence.get_mut("proof").obj().values_mut().next().unwrap().obj()
        };
        if Path::new("synthesized").exists() {
            payload.insert("plan_slug".into(), string("new-plan"));
            payload.insert("artifact_path".into(), string("new/path"));
        } else {
            fs::write("synthesized", "").unwrap();
            payload.insert("plan_slug".into(), string("old-plan"));
            payload.insert("artifact_path".into(), string("old/path"));
        }
    }
    if taskless {
        println!(
            "{}",
            object([
                ("gate".into(), context.get("gate").clone()),
                ("session_id".into(), context.get("session_id").clone()),
                ("decision".into(), string("pass")),
                ("proof".into(), proof),
            ])
            .render()
        );
    } else {
        let root = env::current_dir().unwrap();
        let encoded = evidence.render();
        let status = Command::new(exe)
            .arg("--root")
            .arg(&root)
            .args([
                "task",
                "submit",
                context.get("task_id").string(),
                "--stage",
                context.get("stage").string(),
                "--action",
                "pass",
                "--evidence",
                &encoded,
            ])
            .status()
            .unwrap();
        assert!(status.success());
    }
}

fn failed_artifact_runner(exe: &str, prompt: &str) {
    let context = context(prompt);
    let mut evidence = context
        .get("workflow_guidance")
        .get("expected_evidence_template")
        .clone();
    {
        let payload = evidence.get_mut("proof").obj().values_mut().next().unwrap().obj();
        payload.insert("artifact_path".into(), string("generated/path"));
        payload.insert("plan_slug".into(), string("generated"));
    }
    let root = env::current_dir().unwrap();
    let encoded = evidence.render();
    let status = Command::new(exe)
        .arg("--root")
        .arg(&root)
        .args([
            "task",
            "submit",
            context.get("task_id").string(),
            "--stage",
            context.get("stage").string(),
            "--action",
            "pass",
            "--evidence",
            &encoded,
        ])
        .status()
        .unwrap();
    assert!(status.success());
}

fn capture_role(prompt: &str) {
    let context = context(prompt);
    let mut proof = context.get("expected_response").get("proof").clone();
    fs::write("role-proof.json", proof.render()).unwrap();
    if let Some(Json::Object(review)) = proof.obj().get_mut("review_proof") {
        review.insert("notes".into(), string("Reviewed safety"));
    }
    if let Some(Json::Object(contract)) = proof.obj().get_mut("contract_proof") {
        contract.insert(
            "seam_test_ref".into(),
            string("tests/example.rs::test_case"),
        );
        contract.insert("red_exit_code".into(), Json::Number("101".into()));
    }
    if let Some(Json::Object(cmd)) = proof.obj().get_mut("command_proof") {
        cmd.insert("command".into(), string("fixture"));
    }
    if let Some(Json::Object(seam)) = proof.obj().get_mut("seam_test_proof") {
        seam.insert(
            "seam_test_refs".into(),
            Json::Array(vec![string("tests/example.rs::test_case")]),
        );
    }
    println!(
        "{}",
        object([
            ("gate".into(), context.get("gate").clone()),
            ("session_id".into(), context.get("session_id").clone()),
            ("decision".into(), string("pass")),
            ("proof".into(), proof),
        ])
        .render()
    );
}

fn git_instrumented(git_args: &[String]) {
    let stdin_bytes = if git_args.iter().any(|a| a == "fast-import") {
        let mut buf = Vec::new();
        io::stdin().read_to_end(&mut buf).unwrap();
        Some(buf)
    } else {
        None
    };
    if let Some(bytes) = stdin_bytes.as_deref() {
        let text = String::from_utf8_lossy(bytes);
        if Path::new("fixture-publication-fault").exists()
            && text.contains("refs/acdd/state/acdd/acdd/m-gc/candidate")
            && text.contains("\"intent\":null")
            && text.contains("[\"build\",")
            && text.contains("\"passed\"")
        {
            let lock = Path::new(".git/refs/acdd/state/acdd/acdd/m-gc/candidate.lock");
            fs::create_dir_all(lock.parent().unwrap()).unwrap();
            fs::write(lock, "workspace publication lock").unwrap();
        }
    }
    let mut cmd = Command::new("/usr/bin/git");
    cmd.args(git_args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if stdin_bytes.is_some() {
        cmd.stdin(Stdio::piped());
    }
    let mut child = cmd.spawn().unwrap();
    if let Some(bytes) = stdin_bytes {
        child.stdin.take().unwrap().write_all(&bytes).unwrap();
    }
    let output = child.wait_with_output().unwrap();
    if output.status.success()
        && git_args.iter().any(|a| a == "commit-tree")
        && Path::new("fixture-publication-fault").exists()
        && !git_args.iter().any(|a| a == "-m")
    {
        let sha = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        let show = Command::new("/usr/bin/git")
            .args(["show", &format!("{sha}:checkpoint.json")])
            .output()
            .unwrap();
        if show.status.success() {
            let state = parse(std::str::from_utf8(&show.stdout).unwrap());
            let intent_is_null = matches!(
                state,
                Json::Object(ref m) if matches!(m.get("intent"), Some(Json::Null))
            );
            let build_passed = matches!(
                state,
                Json::Object(ref m) if matches!(
                    m.get("gates"),
                    Some(Json::Array(gates)) if gates.iter().any(|row| {
                        matches!(row, Json::Array(items) if items.len() >= 3 && matches!(&items[0], Json::String(g) if g == "build") && matches!(&items[2], Json::String(s) if s == "passed"))
                    })
                )
            );
            if intent_is_null && build_passed {
                let lock = Path::new(".git/refs/acdd/state/acdd/acdd/m-gc/candidate.lock");
                fs::create_dir_all(lock.parent().unwrap()).unwrap();
                fs::write(lock, "workspace publication lock").unwrap();
            }
        }
    }
    if output.status.success()
        && git_args.first().map(String::as_str) == Some("-c")
        && git_args.iter().any(|a| a == "hash-object")
        && git_args.iter().any(|a| a == "-t")
        && git_args.iter().any(|a| a == "commit")
    {
        let gc = Command::new("/usr/bin/git")
            .args(["gc", "--prune=now"])
            .output()
            .unwrap();
        fs::write(
            "fixture-gc-construction.json",
            object([
                ("gc_attempted".into(), Json::Bool(true)),
                (
                    "exit".into(),
                    Json::Number(gc.status.code().unwrap_or(1).to_string()),
                ),
            ])
            .render(),
        )
        .unwrap();
    }
    if output.status.success()
        && git_args.first().map(String::as_str) == Some("commit-tree")
        && git_args.iter().any(|a| a == "-m")
        && git_args.iter().any(|a| a.starts_with("acdd-snapshot:"))
    {
        let sha = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        assert!(Command::new("/usr/bin/git")
            .args(["reflog", "expire", "--expire=now", "--all"])
            .output()
            .unwrap()
            .status
            .success());
        assert!(Command::new("/usr/bin/git")
            .args(["gc", "--prune=now"])
            .output()
            .unwrap()
            .status
            .success());
        let check = Command::new("/usr/bin/git")
            .args(["cat-file", "-e", &sha])
            .output()
            .unwrap();
        fs::write(
            "fixture-gc-window.json",
            object([
                ("candidate".into(), string(&sha)),
                ("exists_after_gc".into(), Json::Bool(check.status.success())),
                ("gc_attempted".into(), Json::Bool(true)),
            ])
            .render(),
        )
        .unwrap();
    }
    io::stdout().write_all(&output.stdout).unwrap();
    io::stderr().write_all(&output.stderr).unwrap();
    exit(output.status.code().unwrap_or(1));
}

#[cfg(unix)]
fn runner_fixture(mode: &str, arguments: &[String]) {
    const SIGTERM: i32 = 15;
    const SIG_IGN: usize = 1;
    extern "C" {
        fn fork() -> i32;
        fn getpid() -> i32;
        fn getppid() -> i32;
        fn getpgrp() -> i32;
        fn getpgid(pid: i32) -> i32;
        fn setpgid(pid: i32, pgid: i32) -> i32;
        fn alarm(seconds: u32) -> u32;
        fn pause() -> i32;
        fn kill(pid: i32, sig: i32) -> i32;
        fn signal(sig: i32, handler: usize) -> usize;
        fn _exit(status: i32) -> !;
    }
    unsafe {
        alarm(15);
    }
    match mode {
        "inspect" => {
            let cwd = env::current_dir().unwrap();
            let argv = Json::Array(arguments.iter().map(|s| string(s)).collect());
            let out = unsafe {
                object([
                    ("argv".into(), argv),
                    ("cwd".into(), string(&cwd.to_string_lossy())),
                    ("pid".into(), Json::Number(getpid().to_string())),
                    ("pgid".into(), Json::Number(getpgrp().to_string())),
                    ("acdd_session".into(), env_or_null("ACDD_AGENT_SESSION_ID")),
                ])
            };
            println!("{}", out.render());
            io::stdout().flush().unwrap();
        }
        "streams" => {
            let acknowledgment = Path::new(&arguments[0]);
            let o = [b'o'; 8192];
            let e = [b'e'; 8192];
            for _ in 0..32 {
                io::stdout().write_all(&o).unwrap();
                io::stderr().write_all(&e).unwrap();
            }
            io::stdout().flush().unwrap();
            io::stderr().flush().unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            while !acknowledgment.exists() {
                if Instant::now() >= deadline {
                    exit(9);
                }
                thread::sleep(Duration::from_millis(10));
            }
        }
        "hang" | "orphan" => unsafe {
            signal(SIGTERM, SIG_IGN);
            let descendant = fork();
            assert!(descendant >= 0);
            if descendant == 0 {
                alarm(15);
                loop {
                    pause();
                }
            }
            fs::write(&arguments[0], descendant.to_string()).unwrap();
            if mode == "orphan" {
                _exit(0);
            }
            loop {
                pause();
            }
        },
        "wait" => {
            println!("ready");
            io::stdout().flush().unwrap();
            loop {
                unsafe {
                    pause();
                }
            }
        }
        "exit" => exit(7),
        "binary" => {
            let out: Vec<u8> = (0..=255).collect();
            let err: Vec<u8> = (0..=255).rev().collect();
            io::stdout().write_all(&out).unwrap();
            io::stderr().write_all(&err).unwrap();
            io::stdout().flush().unwrap();
            io::stderr().flush().unwrap();
        }
        "signal" => unsafe {
            kill(getpid(), SIGTERM);
        },
        "flood" => {
            let o = [b'o'; 8192];
            let e = [b'e'; 8192];
            loop {
                if io::stdout().write_all(&o).is_err() || io::stderr().write_all(&e).is_err() {
                    break;
                }
            }
        }
        "move" => {
            unsafe {
                setpgid(0, getpgid(getppid()));
            }
            println!("moved");
            io::stdout().flush().unwrap();
            loop {
                unsafe {
                    pause();
                }
            }
        }
        _ => panic!("Unknown fixture mode: {mode}"),
    }
}

fn run() {
    let args: Vec<String> = env::args().collect();
    match args[1].as_str() {
        "task-role" => task_role(&args[2], &args[3], &args[4]),
        "wave-task-role" => {
            let primary = Path::new(&args[2]);
            let root = env::current_dir().unwrap();
            fs::create_dir_all(root.join(".acdd/lifecycle")).unwrap();
            let runner = root.join(".acdd/runner_fixture");
            if !runner.exists() {
                fs::copy(primary.join(".acdd/runner_fixture"), runner).unwrap();
            }
            if primary.join(".acdd/fail-isolated-role").exists() {
                exit(17);
            }
            task_role(&args[3], &args[4], &args[5]);
        }
        "wave-role" => wave_role(&args[2]),
        "workflow-runner" => {
            if args.len() == 3 {
                workflow_runner("default", &args[2]);
            } else {
                workflow_runner(&args[2], &args[3]);
            }
        }
        "polish-runner" => polish_runner(&args[2]),
        "author-runner" => author_runner(&args[2]),
        "artifact-runner" => artifact_runner(&args[2]),
        "artifact-check" => fs::write("validated-arguments", args[2..].join(" ")).unwrap(),
        "plan-artifact-runner" => plan_artifact_runner(&args[2], &args[3]),
        "failed-artifact" => failed_artifact_runner(&args[2], &args[3]),
        "inspect-findings" => {
            let context = context(&args[2]);
            fs::create_dir_all(".acdd").unwrap();
            fs::write(".acdd/repair-context.json", context.render()).unwrap();
            exit(29);
        }
        "capture-role" => capture_role(&args[2]),
        "git-instrumented" => git_instrumented(&args[2..]),
        #[cfg(unix)]
        "runner-fixture" => runner_fixture(&args[2], &args[3..]),
        _ => panic!("invalid mode"),
    }
}

fn main() {
    if std::panic::catch_unwind(run).is_err() {
        exit(1);
    }
}
