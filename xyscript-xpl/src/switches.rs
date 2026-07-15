//! Uniform command-switch syntax for XyScript.
//!
//! A *switch* is written `/NAME` or `-name` (case-insensitive). The syntax is
//! deliberately relaxed and uniform across every command that takes switches:
//!
//! - **Attached to the verb:** `COPY/NV`, `SE/W`, `CD/D`.
//! - **Leading tokens before the positional args:** `COPY /NV a b`, `SE -w "x"`.
//!
//! Switches are recognized only at the verb or *before* the positional arguments,
//! so a path that begins with `/` or `-` (e.g. `/usr/bin`, `-o`) is never mistaken
//! for one — a token only counts as a switch if the command actually declares it
//! (the `is_valid` predicate below).
//!
//! This module owns only the *syntax* (sigils, attachment, position). What a
//! switch *name* means is the command's business: the search verbs treat a name
//! as a group of single-letter flags (`/WA` → `W`,`A`), while word-switch commands
//! match the whole name (`/NV` → `NV`).

/// Split a verb token into its base and any switches attached after `/` or `-`.
///
/// `COPY/NV` → `("COPY", ["NV"])`, `SE/W` → `("SE", ["W"])`,
/// `CD/D` → `("CD", ["D"])`, `INSERT` → `("INSERT", [])`. Names are uppercased;
/// the base is returned as-is (already case-folded by the parser).
pub fn peel_verb(verb: &str) -> (&str, Vec<String>) {
    match verb.find(['/', '-']) {
        None => (verb, Vec::new()),
        Some(i) => {
            let switches = verb[i + 1..]
                .split(['/', '-'])
                .filter(|s| !s.is_empty())
                .map(|s| s.to_ascii_uppercase())
                .collect();
            (&verb[..i], switches)
        }
    }
}

/// The name part of a `/NAME` or `-name` switch token (`None` if it isn't one).
pub fn switch_name(tok: &str) -> Option<&str> {
    let mut chars = tok.chars();
    matches!(chars.next(), Some('/') | Some('-'))
        .then(|| chars.as_str())
        .filter(|s| !s.is_empty())
}

/// Pull leading switch tokens off `args`. A whitespace-delimited token is a switch
/// iff it starts with `/` or `-` and `is_valid` accepts the uppercased name after
/// the sigil. Returns the switch names (uppercased) and the remaining positional
/// argument string (trimmed at the front).
pub fn take_leading(args: &str, is_valid: impl Fn(&str) -> bool) -> (Vec<String>, &str) {
    let mut found = Vec::new();
    let mut rest = args.trim_start();
    while !rest.is_empty() {
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let tok = &rest[..end];
        let Some(name) = switch_name(tok) else { break };
        let name = name.to_ascii_uppercase();
        if !is_valid(&name) {
            break;
        }
        found.push(name);
        rest = rest[end..].trim_start();
    }
    (found, rest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peels_attached_verb_switches() {
        assert_eq!(peel_verb("COPY/NV"), ("COPY", vec!["NV".to_string()]));
        assert_eq!(peel_verb("SE/W"), ("SE", vec!["W".to_string()]));
        assert_eq!(peel_verb("CD/D"), ("CD", vec!["D".to_string()]));
        assert_eq!(peel_verb("X-nv"), ("X", vec!["NV".to_string()]));
        assert_eq!(peel_verb("COPY/NV/F"), ("COPY", vec!["NV".to_string(), "F".to_string()]));
        assert_eq!(peel_verb("INSERT"), ("INSERT", Vec::<String>::new()));
    }

    #[test]
    fn takes_only_valid_leading_switches() {
        let nv = |n: &str| n == "NV";
        // A leading switch is consumed; the rest is positional.
        let (sw, rest) = take_leading("/NV a b", nv);
        assert_eq!(sw, vec!["NV".to_string()]);
        assert_eq!(rest, "a b");
        // A path-like leading token is NOT a switch (NV-only command).
        let (sw, rest) = take_leading("/usr/bin x", nv);
        assert!(sw.is_empty());
        assert_eq!(rest, "/usr/bin x");
        // Dash form.
        let (sw, rest) = take_leading("-nv file", nv);
        assert_eq!(sw, vec!["NV".to_string()]);
        assert_eq!(rest, "file");
    }
}
