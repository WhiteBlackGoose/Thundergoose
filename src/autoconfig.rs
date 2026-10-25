//! Finding the servers for an email address, the way Thunderbird does: well-known presets,
//! the provider's own autoconfig file, Mozilla's ISP database, then the domain's MX record.

use std::time::Duration;

use crate::config::{issuer_for_host, Issuer, Security, Server};

#[derive(Clone, Debug)]
pub struct Detected {
    pub imap: Server,
    pub smtp: Server,
    pub oauth: Option<Issuer>,
    /// Where the settings came from, for the UI.
    pub source: String,
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(8))).build().into()
}

fn get(url: &str) -> Option<String> {
    // Cloudflare's DNS endpoint only answers JSON when asked for exactly that.
    let accept = if url.contains("dns-query") { "application/dns-json" } else { "application/xml, text/xml" };
    let mut resp = agent().get(url).header("Accept", accept).call().ok()?;
    resp.body_mut().read_to_string().ok()
}

fn preset(issuer: Issuer, email: &str) -> Detected {
    let server = |host: &str, port, security| Server { host: host.into(), port, security, username: email.into() };
    match issuer {
        Issuer::Google => Detected {
            imap: server("imap.gmail.com", 993, Security::Tls),
            smtp: server("smtp.gmail.com", 465, Security::Tls),
            oauth: Some(Issuer::Google),
            source: "Google".into(),
        },
        Issuer::Microsoft => Detected {
            imap: server("outlook.office365.com", 993, Security::Tls),
            smtp: server("smtp.office365.com", 587, Security::StartTls),
            oauth: Some(Issuer::Microsoft),
            source: "Microsoft".into(),
        },
    }
}

/// Text of the first `<tag>` inside `block`.
fn tag<'a>(block: &'a str, name: &str) -> Option<&'a str> {
    let start = block.find(&format!("<{name}>"))? + name.len() + 2;
    let end = block[start..].find(&format!("</{name}>"))? + start;
    Some(block[start..end].trim())
}

fn tags<'a>(block: &'a str, name: &str) -> Vec<&'a str> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let mut out = Vec::new();
    let mut rest = block;
    while let Some(i) = rest.find(&open) {
        let after = &rest[i + open.len()..];
        let Some(j) = after.find(&close) else { break };
        out.push(after[..j].trim());
        rest = &after[j + close.len()..];
    }
    out
}

fn server_block<'a>(xml: &'a str, element: &str, kind: &str) -> Option<&'a str> {
    let mut rest = xml;
    loop {
        let i = rest.find(&format!("<{element}"))?;
        let after = &rest[i..];
        let head_end = after.find('>')?;
        let end = after.find(&format!("</{element}>"))?;
        if after[..head_end].contains(&format!("type=\"{kind}\"")) {
            return Some(&after[head_end + 1..end]);
        }
        rest = &after[end..];
    }
}

fn parse_server(block: &str, email: &str, implicit_port: u16) -> Option<Server> {
    let host = tag(block, "hostname")?.to_string();
    let port = tag(block, "port").and_then(|p| p.parse().ok()).unwrap_or(implicit_port);
    let security = match tag(block, "socketType").unwrap_or("") {
        "SSL" => Security::Tls,
        "STARTTLS" => Security::StartTls,
        "plain" => Security::Plain,
        _ if port == implicit_port => Security::Tls,
        _ => Security::StartTls,
    };
    let (local, domain) = email.split_once('@').unwrap_or((email, ""));
    let username = tag(block, "username").unwrap_or("%EMAILADDRESS%").replace("%EMAILADDRESS%", email).replace("%EMAILLOCALPART%", local).replace("%EMAILDOMAIN%", domain);
    Some(Server { host, port, security, username })
}

fn parse_config(xml: &str, email: &str, source: &str) -> Option<Detected> {
    let incoming = server_block(xml, "incomingServer", "imap")?;
    let outgoing = server_block(xml, "outgoingServer", "smtp")?;
    let imap = parse_server(incoming, email, 993)?;
    let smtp = parse_server(outgoing, email, 465)?;
    let oauth = tags(incoming, "authentication").contains(&"OAuth2").then(|| issuer_for_host(&imap.host)).flatten();
    Some(Detected { imap, smtp, oauth, source: source.into() })
}

fn mx_hosts(domain: &str) -> Vec<String> {
    let Some(body) = get(&format!("https://cloudflare-dns.com/dns-query?name={domain}&type=MX")) else { return Vec::new() };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&body) else { return Vec::new() };
    let mut hosts: Vec<(u64, String)> = json["Answer"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| {
            let data = a["data"].as_str()?;
            let (prio, host) = data.split_once(' ')?;
            Some((prio.parse().unwrap_or(0), host.trim_end_matches('.').to_lowercase()))
        })
        .collect();
    hosts.sort();
    hosts.into_iter().map(|(_, h)| h).collect()
}

/// The registrable part of a host name ("mx1.mail.example.co.uk" → "example.co.uk", roughly).
pub fn base_domain(host: &str) -> String {
    let parts: Vec<&str> = host.split('.').collect();
    let n = if parts.len() >= 3 && parts[parts.len() - 2].len() <= 3 { 3 } else { 2 };
    parts[parts.len().saturating_sub(n)..].join(".")
}

pub fn detect(email: &str) -> Detected {
    let email = email.trim();
    let domain = email.rsplit('@').next().unwrap_or("").to_lowercase();
    if let Some(issuer) = issuer_for_host(&domain) {
        return preset(issuer, email);
    }
    let urls = [
        (format!("https://autoconfig.{domain}/mail/config-v1.1.xml?emailaddress={email}"), "the provider"),
        (format!("https://{domain}/.well-known/autoconfig/mail/config-v1.1.xml"), "the provider"),
        (format!("https://autoconfig.thunderbird.net/v1.1/{domain}"), "Thunderbird's ISP database"),
    ];
    for (url, source) in &urls {
        if let Some(d) = get(url).and_then(|xml| parse_config(&xml, email, source)) {
            return d;
        }
    }
    // Hosted domains: the MX record tells who runs the mail.
    let mx = mx_hosts(&domain).into_iter().next();
    if let Some(mx) = mx.clone() {
        if mx.ends_with("google.com") || mx.ends_with("googlemail.com") {
            return Detected { source: "Google Workspace".into(), ..preset(Issuer::Google, email) };
        }
        if mx.ends_with("outlook.com") || mx.ends_with("office365.com") {
            return Detected { source: "Microsoft 365".into(), ..preset(Issuer::Microsoft, email) };
        }
        let base = base_domain(&mx);
        if base != domain {
            if let Some(d) = get(&format!("https://autoconfig.thunderbird.net/v1.1/{base}")).and_then(|xml| parse_config(&xml, email, "Thunderbird's ISP database")) {
                return d;
            }
        }
    }
    // Last resort: probe the usual host names, at the domain and at whoever runs its MX.
    let mut bases = vec![domain.clone()];
    if let Some(mx) = &mx {
        let b = base_domain(mx);
        if !bases.contains(&b) {
            bases.push(b);
        }
    }
    let probe = |prefixes: &[&str], ports: &[(u16, Security)]| {
        for base in &bases {
            for prefix in prefixes {
                let host = format!("{prefix}{base}");
                for (port, security) in ports {
                    if reachable(&host, *port) {
                        return Some(Server { host, port: *port, security: *security, username: email.into() });
                    }
                }
            }
        }
        None
    };
    let imap = probe(&["imap.", "mail.", ""], &[(993, Security::Tls), (143, Security::StartTls)]);
    let smtp = probe(&["smtp.", "mail.", ""], &[(465, Security::Tls), (587, Security::StartTls)]);
    let found = imap.is_some() && smtp.is_some();
    Detected {
        imap: imap.unwrap_or_else(|| Server { host: format!("imap.{domain}"), port: 993, security: Security::Tls, username: email.into() }),
        smtp: smtp.unwrap_or_else(|| Server { host: format!("smtp.{domain}"), port: 465, security: Security::Tls, username: email.into() }),
        oauth: None,
        source: if found { "servers that answered, please check" } else { "a guess, please check" }.into(),
    }
}

fn reachable(host: &str, port: u16) -> bool {
    use std::net::ToSocketAddrs;
    let Ok(addrs) = (host, port).to_socket_addrs() else { return false };
    addrs.into_iter().take(2).any(|a| std::net::TcpStream::connect_timeout(&a, Duration::from_secs(3)).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ispdb() {
        let xml = r#"<clientConfig version="1.1"><emailProvider id="x">
            <incomingServer type="pop3"><hostname>pop.x.org</hostname></incomingServer>
            <incomingServer type="imap"><hostname>imap.x.org</hostname><port>993</port><socketType>SSL</socketType>
              <username>%EMAILLOCALPART%</username><authentication>password-cleartext</authentication></incomingServer>
            <outgoingServer type="smtp"><hostname>smtp.x.org</hostname><port>587</port><socketType>STARTTLS</socketType>
              <username>%EMAILADDRESS%</username></outgoingServer></emailProvider></clientConfig>"#;
        let d = parse_config(xml, "me@x.org", "t").unwrap();
        assert_eq!(d.imap.host, "imap.x.org");
        assert_eq!(d.imap.username, "me");
        assert_eq!(d.smtp.security, Security::StartTls);
        assert_eq!(d.smtp.username, "me@x.org");
        assert!(d.oauth.is_none());
    }
}
