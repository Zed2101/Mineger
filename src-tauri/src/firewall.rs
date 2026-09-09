// src-tauri/src/firewall.rs
//
// Windows Firewall, letto senza privilegi per il test "I tuoi amici riescono
// a entrare?". Le regole in ingresso stanno nel registro
// (`HKLM\SYSTEM\CurrentControlSet\Services\SharedAccess\Parameters\FirewallPolicy\FirewallRules`),
// una stringa `v2.x|Action=Allow|Active=TRUE|Dir=In|Protocol=6|Profile=Private|App=c:\...\java.exe|LPort=25565|Name=...|`
// per regola, indipendente dalla lingua: da lì si capisce se java.exe è
// consentito, bloccato o senza regola sul profilo di rete attivo. `netsh`
// non va bene: stampa testo localizzato.
//
// Aggiungere la regola richiede l'elevazione (UAC): `allow` lancia un
// PowerShell elevato (`Start-Process -Verb RunAs`) che esegue `netsh
// advfirewall firewall add rule` per java.exe sulla porta del server, dopo
// aver tolto le eventuali regole di blocco create dall'avviso di sicurezza.
//
// Una connessione locale all'IP LAN passa dal loopback e NON è filtrata dal
// firewall: prova solo che il server ascolta. Il verdetto sul firewall viene
// dalle regole, non da un connect.

use crate::tr;
use serde::Serialize;
use std::net::Ipv4Addr;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

#[cfg(windows)]
const RULES_KEY: &str = r"SYSTEM\CurrentControlSet\Services\SharedAccess\Parameters\FirewallPolicy\FirewallRules";
/// Tempo massimo concesso a PowerShell per dire quale profilo di rete è attivo.
const PROFILE_TIMEOUT: Duration = Duration::from_secs(5);
/// Tempo massimo per la richiesta UAC + netsh.
const ALLOW_TIMEOUT: Duration = Duration::from_secs(120);

/// Esito della lettura delle regole per il programma del server.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct FirewallInfo {
    /// `allowed` · `blocked` · `no_rule` · `unknown` (lettura fallita) · `n_a` (non Windows)
    pub status: String,
    /// Profilo di rete attivo (`Private` · `Public` · `Domain`), se noto
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Eseguibile a cui si riferisce il verdetto (java.exe del server)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub program: Option<String>,
    /// Nome della regola decisiva, o errore di lettura
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl Default for FirewallInfo {
    fn default() -> Self {
        FirewallInfo { status: "unknown".to_string(), profile: None, program: None, detail: None }
    }
}

/// Una regola del firewall, ridotta a ciò che serve per decidere.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rule {
    pub name: String,
    pub allow: bool,
    pub active: bool,
    pub inbound: bool,
    /// 6 TCP · 17 UDP · None = qualsiasi
    pub protocol: Option<u32>,
    /// Vuoto = tutti i profili
    pub profiles: Vec<String>,
    /// Percorso dell'eseguibile, normalizzato (minuscolo, backslash); None = qualsiasi
    pub app: Option<String>,
    /// Porte locali (intervalli inclusivi); vuoto = tutte
    pub ports: Vec<(u16, u16)>,
}

/// Percorso confrontabile: minuscolo, backslash, variabili d'ambiente espanse.
pub fn normalize_path(p: &str) -> String {
    let expanded = expand_env(p);
    expanded.trim().replace('/', "\\").to_lowercase()
}

/// `%SystemRoot%\x` → `C:\Windows\x` (solo le variabili note al processo).
fn expand_env(p: &str) -> String {
    let mut out = String::new();
    let mut rest = p;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) if end > 0 => {
                let name = &after[..end];
                match std::env::var(name) {
                    Ok(v) => out.push_str(&v),
                    Err(_) => {
                        out.push('%');
                        out.push_str(name);
                        out.push('%');
                    }
                }
                rest = &after[end + 1..];
            }
            _ => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

fn parse_port_spec(spec: &str) -> Option<(u16, u16)> {
    let s = spec.trim();
    if let Some((a, b)) = s.split_once('-') {
        return Some((a.trim().parse().ok()?, b.trim().parse().ok()?));
    }
    let p: u16 = s.parse().ok()?;
    Some((p, p))
}

/// Interpreta il valore di registro di una regola. `None` se non è una regola v2.
pub fn parse_rule(value: &str) -> Option<Rule> {
    let mut parts = value.split('|');
    let version = parts.next()?.trim();
    if !version.starts_with('v') {
        return None;
    }
    let mut rule = Rule::default();
    let mut seen_action = false;
    for part in parts {
        let Some((k, v)) = part.split_once('=') else { continue };
        match k {
            "Action" => {
                seen_action = true;
                rule.allow = v.eq_ignore_ascii_case("Allow");
            }
            "Active" => rule.active = v.eq_ignore_ascii_case("TRUE"),
            "Dir" => rule.inbound = v.eq_ignore_ascii_case("In"),
            "Protocol" => rule.protocol = v.trim().parse().ok(),
            "Profile" => rule.profiles.push(v.trim().to_string()),
            "App" => rule.app = Some(normalize_path(v)),
            "LPort" => {
                // Le parole chiave (RPC, IPHTTPS, Ply2Disc…) non riguardano Minecraft: regola su porte "speciali", mai la nostra.
                match parse_port_spec(v) {
                    Some(range) => rule.ports.push(range),
                    None => rule.ports.push((0, 0)),
                }
            }
            "Name" => rule.name = v.to_string(),
            _ => {}
        }
    }
    seen_action.then_some(rule)
}

fn profile_matches(rule: &Rule, profile: Option<&str>) -> bool {
    if rule.profiles.is_empty() {
        return true;
    }
    match profile {
        // Profilo sconosciuto: si considera la regola valida (meglio "consentito" a torto che un falso allarme)
        None => true,
        Some(p) => rule.profiles.iter().any(|x| x.eq_ignore_ascii_case(p) || (p.eq_ignore_ascii_case("DomainAuthenticated") && x.eq_ignore_ascii_case("Domain"))),
    }
}

fn port_matches(rule: &Rule, port: u16) -> bool {
    rule.ports.is_empty() || rule.ports.iter().any(|(a, b)| *a <= port && port <= *b)
}

/// La regola riguarda il nostro programma/porta in ingresso su TCP?
fn targets(rule: &Rule, program: Option<&str>, port: u16, profile: Option<&str>) -> bool {
    if !rule.active || !rule.inbound || !matches!(rule.protocol, None | Some(6) | Some(256)) || !profile_matches(rule, profile) {
        return false;
    }
    match (&rule.app, program) {
        (Some(app), Some(prog)) => app == prog && port_matches(rule, port),
        // Regola senza programma: conta solo se nomina esplicitamente la porta
        (None, _) => !rule.ports.is_empty() && port_matches(rule, port),
        // Programma sconosciuto: regole di altri programmi non dicono nulla
        (Some(_), None) => false,
    }
}

/// `allowed` · `blocked` · `no_rule` per (programma, porta) sul profilo dato.
/// Un blocco esplicito vince sempre su un consenso (semantica di Windows).
pub fn evaluate(rules: &[Rule], program: Option<&str>, port: u16, profile: Option<&str>) -> (String, Option<String>) {
    let program = program.map(normalize_path);
    let relevant: Vec<&Rule> = rules.iter().filter(|r| targets(r, program.as_deref(), port, profile)).collect();
    if let Some(block) = relevant.iter().find(|r| !r.allow) {
        return ("blocked".to_string(), Some(block.name.clone()));
    }
    if let Some(allow) = relevant.iter().find(|r| r.allow) {
        return ("allowed".to_string(), Some(allow.name.clone()));
    }
    ("no_rule".to_string(), None)
}

/// Nomi delle regole di blocco in ingresso per il programma (da togliere prima di consentire).
pub fn block_rule_names(rules: &[Rule], program: &str) -> Vec<String> {
    let program = normalize_path(program);
    rules
        .iter()
        .filter(|r| !r.allow && r.inbound && r.app.as_deref() == Some(program.as_str()) && !r.name.is_empty())
        .map(|r| r.name.clone())
        .collect()
}

#[cfg(windows)]
pub fn read_rules() -> Result<Vec<Rule>, String> {
    use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_READ};
    use winreg::types::FromRegValue;
    use winreg::RegKey;
    let key = RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey_with_flags(RULES_KEY, KEY_READ)
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for (_, value) in key.enum_values().filter_map(Result::ok) {
        if let Ok(s) = String::from_reg_value(&value) {
            if let Some(rule) = parse_rule(&s) {
                out.push(rule);
            }
        }
    }
    Ok(out)
}

#[cfg(not(windows))]
pub fn read_rules() -> Result<Vec<Rule>, String> {
    Err("not windows".to_string())
}

/// Esegue PowerShell nascosto e ritorna stdout; errore con stderr se esce ≠ 0.
#[cfg(windows)]
fn powershell(script: &str, timeout: Duration) -> Result<String, String> {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut cmd = Command::new("powershell.exe");
    cmd.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW);
    let child = cmd.spawn().map_err(|e| e.to_string())?;
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    match rx.recv_timeout(timeout) {
        Ok(Ok(out)) => {
            let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if out.status.success() {
                Ok(stdout)
            } else {
                let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
                Err(if stderr.is_empty() { stdout } else { stderr })
            }
        }
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err("timeout".to_string()),
    }
}

/// Profilo di rete (`Private` · `Public` · `DomainAuthenticated`) dell'interfaccia con l'IP dato.
#[cfg(windows)]
pub fn active_profile(lan_ip: Option<Ipv4Addr>) -> Option<String> {
    let filter = match lan_ip {
        Some(ip) => format!(
            "$i = (Get-NetIPAddress -IPAddress '{}' -ErrorAction SilentlyContinue | Select-Object -First 1).InterfaceIndex; ",
            ip
        ),
        None => "$i = $null; ".to_string(),
    };
    let script = format!(
        "{filter}(Get-NetConnectionProfile -ErrorAction SilentlyContinue | Where-Object {{ $null -eq $i -or $_.InterfaceIndex -eq $i }} | Select-Object -First 1).NetworkCategory"
    );
    let out = powershell(&script, PROFILE_TIMEOUT).ok()?;
    let value = out.lines().last()?.trim().to_string();
    (!value.is_empty()).then_some(value)
}

#[cfg(not(windows))]
pub fn active_profile(_lan_ip: Option<Ipv4Addr>) -> Option<String> {
    None
}

/// Verdetto sul firewall per il programma del server sulla sua porta.
pub fn check(program: Option<&str>, port: u16, lan_ip: Option<Ipv4Addr>) -> FirewallInfo {
    if !cfg!(windows) {
        return FirewallInfo { status: "n_a".to_string(), profile: None, program: program.map(str::to_string), detail: None };
    }
    // Profilo e regole in parallelo: il profilo passa da PowerShell (lento), le regole dal registro (istantaneo).
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(active_profile(lan_ip));
    });
    let rules = read_rules();
    let profile = rx.recv_timeout(PROFILE_TIMEOUT + Duration::from_millis(500)).ok().flatten();
    match rules {
        Ok(rules) => {
            let (status, detail) = evaluate(&rules, program, port, profile.as_deref());
            FirewallInfo { status, profile, program: program.map(str::to_string), detail }
        }
        Err(e) => FirewallInfo { status: "unknown".to_string(), profile, program: program.map(str::to_string), detail: Some(e) },
    }
}

/// Testo dello script PowerShell (eseguito elevato) che toglie i blocchi e aggiunge il consenso.
pub fn allow_script(port: u16, program: Option<&str>, block_names: &[String]) -> String {
    let q = |s: &str| format!("'{}'", s.replace('\'', "''"));
    let mut lines = vec!["$ErrorActionPreference = 'Continue'".to_string()];
    if let Some(prog) = program {
        for name in block_names {
            lines.push(format!("netsh advfirewall firewall delete rule name={} dir=in program={} | Out-Null", q(name), q(prog)));
        }
    }
    let mut add = format!(
        "netsh advfirewall firewall add rule name={} dir=in action=allow protocol=TCP localport={} profile=private,public enable=yes",
        q(&format!("Mineger Minecraft {}", port)),
        port
    );
    if let Some(prog) = program {
        add.push_str(&format!(" program={}", q(prog)));
    }
    lines.push(add);
    lines.push("exit $LASTEXITCODE".to_string());
    lines.join("\n")
}

/// Aggiunge la regola di consenso (UAC). Solo in locale: non esposta all'host remoto.
#[cfg(windows)]
pub fn allow(port: u16, program: Option<&str>) -> Result<FirewallInfo, String> {
    use base64::Engine;
    let program = program.map(normalize_path).filter(|p| p.ends_with(".exe"));
    let block_names = match (&program, read_rules()) {
        (Some(p), Ok(rules)) => block_rule_names(&rules, p),
        _ => Vec::new(),
    };
    let script = allow_script(port, program.as_deref(), &block_names);
    let utf16: Vec<u8> = script.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    let encoded = base64::engine::general_purpose::STANDARD.encode(utf16);
    let outer = format!(
        "$p = Start-Process -FilePath 'powershell.exe' -ArgumentList @('-NoProfile','-NonInteractive','-ExecutionPolicy','Bypass','-WindowStyle','Hidden','-EncodedCommand','{encoded}') -Verb RunAs -Wait -PassThru -WindowStyle Hidden; exit $p.ExitCode"
    );
    match powershell(&outer, ALLOW_TIMEOUT) {
        Ok(_) => {}
        Err(e) => {
            let lower = e.to_lowercase();
            if lower.contains("cancel") || lower.contains("annull") || lower.contains("1223") {
                return Err(tr!("errors.reach.firewall_declined"));
            }
            return Err(tr!("errors.reach.firewall_failed", "error" => e));
        }
    }
    let info = check(program.as_deref(), port, crate::upnp::local_ip());
    if info.status == "allowed" {
        Ok(info)
    } else {
        Err(tr!("errors.reach.firewall_failed", "error" => info.detail.clone().unwrap_or_else(|| info.status.clone())))
    }
}

#[cfg(not(windows))]
pub fn allow(_port: u16, _program: Option<&str>) -> Result<FirewallInfo, String> {
    Err(tr!("errors.reach.firewall_unsupported"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const JAVA: &str = r"C:\Program Files\Java\jdk-21\bin\java.exe";

    fn rule(s: &str) -> Rule {
        parse_rule(s).expect("regola valida")
    }

    #[test]
    fn parses_registry_rule_strings() {
        let r = rule(r"v2.10|Action=Allow|Active=TRUE|Dir=In|Protocol=6|Profile=Private|Profile=Public|App=c:\program files\java\jdk-21\bin\java.exe|Name=java.exe|Desc=java.exe|Defer=User|");
        assert!(r.allow && r.active && r.inbound);
        assert_eq!(r.protocol, Some(6));
        assert_eq!(r.profiles, vec!["Private", "Public"]);
        assert_eq!(r.app.as_deref(), Some(r"c:\program files\java\jdk-21\bin\java.exe"));
        assert!(r.ports.is_empty());
        assert_eq!(r.name, "java.exe");

        let b = rule(r"v2.33|Action=Block|Active=TRUE|Dir=In|Protocol=6|Profile=Public|App=C:\Program Files\Java\jdk-21\bin\java.exe|Name=TCP Query User{8B5D1C6E-0000-4000-8000-000000000001}C:\program files\java\jdk-21\bin\java.exe|Desc=java.exe|Defer=User|");
        assert!(!b.allow);
        assert_eq!(b.app.as_deref(), Some(r"c:\program files\java\jdk-21\bin\java.exe"), "il percorso viene normalizzato in minuscolo");

        let p = rule("v2.31|Action=Allow|Active=TRUE|Dir=In|Protocol=6|LPort=25565|Name=Mineger Minecraft 25565|");
        assert_eq!(p.ports, vec![(25565, 25565)]);
        assert!(p.app.is_none());
        let range = rule("v2.31|Action=Allow|Active=TRUE|Dir=In|Protocol=6|LPort=25500-25600|Name=x|");
        assert_eq!(range.ports, vec![(25500, 25600)]);
        let keyword = rule("v2.31|Action=Allow|Active=TRUE|Dir=In|Protocol=6|LPort=RPC|Name=x|");
        assert_eq!(keyword.ports, vec![(0, 0)]);

        assert!(parse_rule("garbage").is_none());
        assert!(parse_rule("v2.10|Active=TRUE|Dir=In|").is_none(), "senza Action non è una regola");
    }

    #[test]
    fn allow_rule_for_java_means_allowed() {
        let rules = vec![rule(r"v2.10|Action=Allow|Active=TRUE|Dir=In|Protocol=6|Profile=Private|Profile=Public|App=c:\program files\java\jdk-21\bin\java.exe|Name=java.exe|")];
        assert_eq!(evaluate(&rules, Some(JAVA), 25565, Some("Private")).0, "allowed");
        assert_eq!(evaluate(&rules, Some(JAVA), 25565, Some("Public")).0, "allowed");
        assert_eq!(evaluate(&rules, Some(JAVA), 25565, None).0, "allowed");
        // Una Java diversa non è coperta
        assert_eq!(evaluate(&rules, Some(r"C:\Java17\bin\java.exe"), 25565, Some("Private")).0, "no_rule");
    }

    #[test]
    fn block_wins_over_allow() {
        let rules = vec![
            rule(r"v2.10|Action=Allow|Active=TRUE|Dir=In|Protocol=6|App=c:\program files\java\jdk-21\bin\java.exe|Name=java.exe|"),
            rule(r"v2.10|Action=Block|Active=TRUE|Dir=In|Protocol=6|App=c:\program files\java\jdk-21\bin\java.exe|Name=TCP Query User{X}java|"),
        ];
        let (status, detail) = evaluate(&rules, Some(JAVA), 25565, Some("Private"));
        assert_eq!(status, "blocked");
        assert_eq!(detail.as_deref(), Some("TCP Query User{X}java"));
        assert_eq!(block_rule_names(&rules, JAVA), vec!["TCP Query User{X}java".to_string()]);
    }

    #[test]
    fn private_only_rule_does_nothing_on_public_network() {
        let rules = vec![rule(r"v2.10|Action=Allow|Active=TRUE|Dir=In|Protocol=6|Profile=Private|App=c:\program files\java\jdk-21\bin\java.exe|Name=java.exe|")];
        assert_eq!(evaluate(&rules, Some(JAVA), 25565, Some("Private")).0, "allowed");
        assert_eq!(evaluate(&rules, Some(JAVA), 25565, Some("Public")).0, "no_rule");
        assert_eq!(evaluate(&rules, Some(JAVA), 25565, Some("DomainAuthenticated")).0, "no_rule");
        let domain = vec![rule(r"v2.10|Action=Allow|Active=TRUE|Dir=In|Protocol=6|Profile=Domain|App=c:\program files\java\jdk-21\bin\java.exe|Name=java.exe|")];
        assert_eq!(evaluate(&domain, Some(JAVA), 25565, Some("DomainAuthenticated")).0, "allowed");
    }

    #[test]
    fn inactive_outbound_udp_and_foreign_port_rules_are_ignored() {
        let rules = vec![
            rule(r"v2.10|Action=Allow|Active=FALSE|Dir=In|Protocol=6|App=c:\program files\java\jdk-21\bin\java.exe|Name=off|"),
            rule(r"v2.10|Action=Allow|Active=TRUE|Dir=Out|Protocol=6|App=c:\program files\java\jdk-21\bin\java.exe|Name=out|"),
            rule(r"v2.10|Action=Allow|Active=TRUE|Dir=In|Protocol=17|App=c:\program files\java\jdk-21\bin\java.exe|Name=udp|"),
            rule(r"v2.10|Action=Allow|Active=TRUE|Dir=In|Protocol=6|LPort=25566|App=c:\program files\java\jdk-21\bin\java.exe|Name=other port|"),
            rule(r"v2.10|Action=Block|Active=TRUE|Dir=In|Protocol=6|LPort=RPC|Name=rpc keyword|"),
        ];
        assert_eq!(evaluate(&rules, Some(JAVA), 25565, Some("Private")).0, "no_rule");
    }

    #[test]
    fn port_only_rules_count_without_program() {
        let rules = vec![rule("v2.31|Action=Allow|Active=TRUE|Dir=In|Protocol=6|LPort=25565|Name=Mineger Minecraft 25565|")];
        assert_eq!(evaluate(&rules, Some(JAVA), 25565, Some("Public")).0, "allowed");
        assert_eq!(evaluate(&rules, None, 25565, Some("Public")).0, "allowed");
        assert_eq!(evaluate(&rules, None, 25566, Some("Public")).0, "no_rule");
        let broad = vec![rule("v2.31|Action=Allow|Active=TRUE|Dir=In|Protocol=6|Name=everything|")];
        assert_eq!(evaluate(&broad, Some(JAVA), 25565, None).0, "no_rule", "una regola senza programma né porta non conta");
        let any_proto = vec![rule("v2.31|Action=Allow|Active=TRUE|Dir=In|LPort=25565|Name=any protocol|")];
        assert_eq!(evaluate(&any_proto, None, 25565, None).0, "allowed");
    }

    #[test]
    fn paths_are_normalized_and_env_expanded() {
        assert_eq!(normalize_path(r"C:/Program Files/Java/BIN/java.exe"), r"c:\program files\java\bin\java.exe");
        unsafe { std::env::set_var("MINEGER_TEST_ROOT", r"D:\Apps") };
        assert_eq!(normalize_path(r"%MINEGER_TEST_ROOT%\java.exe"), r"d:\apps\java.exe");
        assert_eq!(normalize_path(r"%MINEGER_NOT_SET_XYZ%\java.exe"), r"%mineger_not_set_xyz%\java.exe");
    }

    /// Dipende dalla macchina (registro + PowerShell): `cargo test --lib firewall -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn reads_real_rules_and_profile() {
        let rules = read_rules().expect("lettura del registro");
        let java: Vec<&Rule> = rules.iter().filter(|r| r.app.as_deref().map(|a| a.ends_with("java.exe")).unwrap_or(false)).collect();
        println!("{} regole, {} per java.exe", rules.len(), java.len());
        for r in &java {
            println!("  {} allow={} active={} in={} proto={:?} profiles={:?} app={:?}", r.name, r.allow, r.active, r.inbound, r.protocol, r.profiles, r.app);
        }
        let profile = active_profile(crate::upnp::local_ip());
        println!("profilo attivo: {:?}", profile);
        if let Some(first) = java.first() {
            let app = first.app.clone().unwrap();
            println!("verdetto per {}: {:?}", app, evaluate(&rules, Some(&app), 25565, profile.as_deref()));
        }
        assert!(!rules.is_empty());
    }

    #[test]
    fn allow_script_quotes_and_removes_blocks() {
        let s = allow_script(25565, Some(r"C:\Program Files\Java\bin\java.exe"), &["TCP Query User{1}c:\\it's".to_string()]);
        assert!(s.contains("delete rule name='TCP Query User{1}c:\\it''s' dir=in program='C:\\Program Files\\Java\\bin\\java.exe'"));
        assert!(s.contains("add rule name='Mineger Minecraft 25565' dir=in action=allow protocol=TCP localport=25565 profile=private,public enable=yes program='C:\\Program Files\\Java\\bin\\java.exe'"));
        assert!(s.ends_with("exit $LASTEXITCODE"));
        let port_only = allow_script(25565, None, &[]);
        assert!(!port_only.contains("delete rule") && !port_only.contains("program="));
    }
}
