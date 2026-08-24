//! The key pattern DSL, implemented as a segment-aware backtracking matcher.
//!
//! ref: lib/i18n/tasks/key_pattern_matching.rb:26-39
//!
//! The gem compiles each pattern to a Ruby regex:
//!
//! ```text
//! .gsub(".",  '\.')
//! .gsub("*:", "[^.]+?")
//! .gsub("*",  ".*")
//! .gsub(":",  '(?<=^|\.)[^.]+?(?=\.|$)')
//! .gsub(/\{(.*?)}/) { "(#{$1.strip.gsub(/\s*,\s*/, "|")})" }
//! ```
//!
//! Two of those constructs are out of reach for the `regex` crate: the
//! lookaround in `:` and the numbered backreferences that `data.write` needs.
//! See blocker B2. So the pattern compiles to a small backtracking program
//! instead, run directly over the bytes of the dotted key.

/// One instruction of the compiled pattern program.
#[derive(Debug, Clone)]
enum Inst {
    /// Literal bytes. A `.` inside is a segment separator, matched literally.
    Lit(Box<[u8]>),
    /// `*` — any bytes, dots included. Greedy, like Ruby `.*`.
    Star,
    /// `*:` — one or more non-dot bytes. Non-greedy, like Ruby `[^.]+?`.
    PartialSeg,
    /// `:` — exactly one whole segment, both boundaries asserted.
    WholeSeg,
    /// Try each target program counter in order.
    Split(Box<[usize]>),
    Jmp(usize),
    CapStart(usize),
    CapEnd(usize),
    /// Succeed only at the end of the key, mirroring the gem's `\A...\z`.
    Match,
}

/// A single compiled key pattern.
#[derive(Debug, Clone)]
pub struct Pattern {
    prog: Vec<Inst>,
    /// Literal bytes the key must start with. A cheap reject for long
    /// `ignore_*` lists.
    prefix: Box<[u8]>,
    pub group_count: usize,
    pub source: String,
}

/// A half-open byte range inside the key, for one `{...}` capture group.
pub type Captures = Vec<Option<(usize, usize)>>;

impl Pattern {
    pub fn compile(source: &str) -> Pattern {
        let mut group_count = 0;
        let toks = tokenize(source, &mut group_count);
        let mut prog = Vec::new();
        emit(&toks, &mut prog);
        prog.push(Inst::Match);
        let prefix: Box<[u8]> = match prog.first() {
            Some(Inst::Lit(l)) => l.clone(),
            _ => Box::from(&[][..]),
        };
        Pattern {
            prog,
            prefix,
            group_count,
            source: source.to_string(),
        }
    }

    pub fn is_match(&self, key: &str) -> bool {
        self.captures(key).is_some()
    }

    /// Returns the `{...}` group captures when the pattern matches the whole key.
    pub fn captures(&self, key: &str) -> Option<Captures> {
        let bytes = key.as_bytes();
        if !bytes.starts_with(&self.prefix) {
            return None;
        }
        let mut caps: Captures = vec![None; self.group_count];
        if self.run(0, bytes, 0, &mut caps) {
            Some(caps)
        } else {
            None
        }
    }

    fn run(&self, pc: usize, s: &[u8], pos: usize, caps: &mut Captures) -> bool {
        match &self.prog[pc] {
            Inst::Match => pos == s.len(),
            Inst::Lit(l) => {
                if s.len() - pos >= l.len() && s[pos..pos + l.len()] == l[..] {
                    self.run(pc + 1, s, pos + l.len(), caps)
                } else {
                    false
                }
            }
            // Greedy, so the longest run is tried first.
            Inst::Star => {
                let mut end = s.len();
                loop {
                    if self.run(pc + 1, s, end, caps) {
                        return true;
                    }
                    if end == pos {
                        return false;
                    }
                    end -= 1;
                }
            }
            // Non-greedy, so the shortest run is tried first. At least one byte.
            Inst::PartialSeg => {
                let mut end = pos;
                while end < s.len() && s[end] != b'.' {
                    end += 1;
                    if self.run(pc + 1, s, end, caps) {
                        return true;
                    }
                }
                false
            }
            // The lookbehind and lookahead leave exactly one candidate: the
            // whole segment starting at `pos`.
            Inst::WholeSeg => {
                if pos != 0 && s[pos - 1] != b'.' {
                    return false;
                }
                let mut end = pos;
                while end < s.len() && s[end] != b'.' {
                    end += 1;
                }
                if end == pos {
                    return false;
                }
                self.run(pc + 1, s, end, caps)
            }
            Inst::Split(targets) => {
                for &t in targets.iter() {
                    if self.run(t, s, pos, caps) {
                        return true;
                    }
                }
                false
            }
            Inst::Jmp(t) => self.run(*t, s, pos, caps),
            // Save and restore so a failed branch leaves no stale capture.
            Inst::CapStart(i) => {
                let saved = caps[*i];
                caps[*i] = Some((pos, pos));
                if self.run(pc + 1, s, pos, caps) {
                    true
                } else {
                    caps[*i] = saved;
                    false
                }
            }
            Inst::CapEnd(i) => {
                let saved = caps[*i];
                let start = saved.map_or(pos, |(s0, _)| s0);
                caps[*i] = Some((start, pos));
                if self.run(pc + 1, s, pos, caps) {
                    true
                } else {
                    caps[*i] = saved;
                    false
                }
            }
        }
    }
}

/// A list of patterns. Matches when any member matches.
#[derive(Debug, Clone, Default)]
pub struct PatternSet {
    pats: Vec<Pattern>,
}

impl PatternSet {
    /// An empty list matches nothing. ref: `MATCH_NOTHING = /\z\A/`.
    pub fn new<S: AsRef<str>>(sources: &[S]) -> PatternSet {
        PatternSet {
            pats: sources
                .iter()
                .map(|s| Pattern::compile(s.as_ref()))
                .collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.pats.is_empty()
    }

    pub fn is_match(&self, key: &str) -> bool {
        self.pats.iter().any(|p| p.is_match(key))
    }

    /// The first matching pattern together with its captures.
    pub fn first_match(&self, key: &str) -> Option<(&Pattern, Captures)> {
        self.pats
            .iter()
            .find_map(|p| p.captures(key).map(|c| (p, c)))
    }
}

#[derive(Debug, Clone)]
enum Tok {
    Lit(String),
    Star,
    PartialSeg,
    WholeSeg,
    Group(usize, Vec<Vec<Tok>>),
}

/// Scans left to right, checking `*:` before `*` before `:`, which reproduces
/// the gem's `gsub` order.
fn tokenize(src: &str, next_group: &mut usize) -> Vec<Tok> {
    let b = src.as_bytes();
    let mut out: Vec<Tok> = Vec::new();
    let mut lit = String::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            // The gem's `/\{(.*?)}/` is non-greedy and does not nest, so the
            // group ends at the first `}`. An unclosed `{` stays literal.
            b'{' => match src[i + 1..].find('}') {
                Some(close) => {
                    if !lit.is_empty() {
                        out.push(Tok::Lit(std::mem::take(&mut lit)));
                    }
                    let body = &src[i + 1..i + 1 + close];
                    let gi = *next_group;
                    *next_group += 1;
                    let alts = body
                        .trim()
                        .split(',')
                        .map(|a| tokenize(a.trim(), next_group))
                        .collect();
                    out.push(Tok::Group(gi, alts));
                    i += close + 2;
                }
                None => {
                    lit.push('{');
                    i += 1;
                }
            },
            b'*' => {
                if !lit.is_empty() {
                    out.push(Tok::Lit(std::mem::take(&mut lit)));
                }
                if b.get(i + 1) == Some(&b':') {
                    out.push(Tok::PartialSeg);
                    i += 2;
                } else {
                    out.push(Tok::Star);
                    i += 1;
                }
            }
            b':' => {
                if !lit.is_empty() {
                    out.push(Tok::Lit(std::mem::take(&mut lit)));
                }
                out.push(Tok::WholeSeg);
                i += 1;
            }
            _ => {
                let ch = src[i..].chars().next().unwrap();
                lit.push(ch);
                i += ch.len_utf8();
            }
        }
    }
    if !lit.is_empty() {
        out.push(Tok::Lit(lit));
    }
    out
}

fn emit(toks: &[Tok], prog: &mut Vec<Inst>) {
    for tok in toks {
        match tok {
            Tok::Lit(l) => prog.push(Inst::Lit(Box::from(l.as_bytes()))),
            Tok::Star => prog.push(Inst::Star),
            Tok::PartialSeg => prog.push(Inst::PartialSeg),
            Tok::WholeSeg => prog.push(Inst::WholeSeg),
            Tok::Group(gi, alts) => {
                prog.push(Inst::CapStart(*gi));
                let split_at = prog.len();
                prog.push(Inst::Split(Box::from(&[][..])));
                let mut starts = Vec::with_capacity(alts.len());
                let mut jmps = Vec::with_capacity(alts.len());
                for alt in alts {
                    starts.push(prog.len());
                    emit(alt, prog);
                    jmps.push(prog.len());
                    prog.push(Inst::Jmp(usize::MAX));
                }
                let end = prog.len();
                prog[split_at] = Inst::Split(starts.into_boxed_slice());
                for j in jmps {
                    prog[j] = Inst::Jmp(end);
                }
                prog.push(Inst::CapEnd(*gi));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(pat: &str, key: &str) -> bool {
        Pattern::compile(pat).is_match(key)
    }

    // Full port of spec/key_pattern_matching_spec.rb
    #[test]
    fn star_as_suffix() {
        assert!(m("devise.*", "devise.some.key"));
    }

    #[test]
    fn star_as_prefix() {
        assert!(m("*.some.key", "devise.some.key"));
    }

    #[test]
    fn star_as_infix() {
        assert!(m("*.some.*", "devise.some.key"));
    }

    #[test]
    fn star_matches_multiple_namespaces() {
        assert!(m("a.*.e*", "a.b.c.d.eeee"));
    }

    #[test]
    fn colon_as_suffix() {
        assert!(m("a.b.:", "a.b.c"));
        assert!(!m("a.b.:", "a.b.c.d"));
    }

    #[test]
    fn colon_as_prefix() {
        assert!(m(":.b.c", "a.b.c"));
        assert!(!m(":.b.c", "x.a.b.c"));
    }

    #[test]
    fn colon_as_infix() {
        assert!(m("a.:.c", "a.b.c"));
        assert!(!m("a.:.c", "a.b.x.c"));
    }

    #[test]
    fn partial_seg_as_suffix() {
        assert!(m("a.b.pre-*:-post", "a.b.pre-c-post"));
        assert!(!m("a.b.pre-*:-post", "a.b.pre-c.-post"));
    }

    #[test]
    fn partial_seg_as_prefix() {
        assert!(m("pre-*:-post.b.c", "pre-a-post.b.c"));
        assert!(!m("pre-*:-post.b.c", "pre-.a-post.b.c"));
    }

    #[test]
    fn partial_seg_as_infix() {
        assert!(m("a.pre-*:-post.c", "a.pre-b-post.c"));
        assert!(!m("a.pre-*:-post.c", "a.pre-b.-post.c"));
    }

    #[test]
    fn sets_match() {
        assert!(m("a.{x,y}.b", "a.x.b"));
        assert!(m("a.{x,y}.b", "a.y.b"));
        assert!(!m("a.{x,y}.b", "a.z.b"));
    }

    #[test]
    fn sets_support_colon() {
        assert!(m("a.{:}.c", "a.b.c"));
        assert!(!m("a.{:}.c", "a.b.x.c"));
    }

    #[test]
    fn sets_support_star() {
        assert!(m("a.{*}.c", "a.b.c"));
        assert!(m("a.{*}.c", "a.b.x.y.c"));
    }

    #[test]
    fn sets_capture() {
        let p = Pattern::compile("a.{x,y}.{:}");
        let key = "a.x.c";
        let caps = p.captures(key).unwrap();
        let got: Vec<&str> = caps
            .iter()
            .map(|c| {
                let (s, e) = c.unwrap();
                &key[s..e]
            })
            .collect();
        assert_eq!(got, vec!["x", "c"]);
    }

    // Additional coverage beyond the gem spec.

    #[test]
    fn empty_set_matches_nothing() {
        let empty: [&str; 0] = [];
        assert!(!PatternSet::new(&empty).is_match("a.b"));
        assert!(PatternSet::new(&empty).is_empty());
    }

    #[test]
    fn colon_needs_segment_boundaries() {
        // `(?=\.|$)` forbids a literal directly after a whole segment.
        assert!(!m(":x", "aax"));
        assert!(m(":.x", "aa.x"));
    }

    #[test]
    fn captures_feed_the_pattern_router() {
        // ref: lib/i18n/tasks/data/router/pattern_router.rb
        let p = Pattern::compile("{activemodel, activerecord, views}.*");
        let key = "activerecord.models.user";
        let caps = p.captures(key).unwrap();
        assert_eq!(caps.len(), 1);
        let (s, e) = caps[0].unwrap();
        assert_eq!(&key[s..e], "activerecord");
    }

    #[test]
    fn no_wildcard_matches_only_itself() {
        for key in ["a", "a.b", "a.b.c", "x.y.z.w"] {
            let p = Pattern::compile(key);
            assert!(p.is_match(key));
            assert!(!p.is_match(&format!("{key}.extra")));
            assert!(!p.is_match(&format!("extra.{key}")));
        }
    }

    #[test]
    fn a_star_that_cannot_be_satisfied_fails() {
        // `*` is greedy and backtracks a byte at a time; here nothing works.
        assert!(!m("a.*.zzz", "a.b.c"));
        assert!(!m("a.*", "b.c"));
        // `*` matches the empty string, so the suffix may sit flush.
        assert!(m("a.*b", "a.b"));
    }

    #[test]
    fn a_whole_segment_must_start_on_a_boundary() {
        // `:` after a literal that does not end in a dot can never match: the
        // gem's lookbehind is `(?<=\A|\.)`.
        assert!(!m("a:", "ab"));
        assert!(!m("a:", "a.b"));
        // The same pattern with the dot in place does match.
        assert!(m("a.:", "a.b"));
        // A whole segment needs at least one character, so a trailing dot in
        // the key leaves nothing for it.
        assert!(!m("a.:", "a."));
        assert!(!m(":", ""));
    }

    #[test]
    fn a_pattern_set_reports_the_first_match_with_its_captures() {
        let set = PatternSet::new(&["nope.*".to_string(), "{a,b}.*".to_string()]);
        assert!(!set.is_empty());
        let key = "b.deep.key";
        let (pat, caps) = set.first_match(key).expect("the second pattern matches");
        assert_eq!(pat.source, "{a,b}.*");
        let (s, e) = caps[0].unwrap();
        assert_eq!(&key[s..e], "b");
        assert!(set.first_match("elsewhere.key").is_none());
    }

    #[test]
    fn unclosed_brace_is_literal() {
        assert!(m("a{b", "a{b"));
    }

    #[test]
    fn real_project_ignore_patterns() {
        // Entries taken from a real-world config.
        assert!(m(
            "{activerecord, activemodel, number}.*",
            "activerecord.models.user"
        ));
        assert!(m("badges.{alt,tooltip}.*", "badges.alt.verified"));
        assert!(!m("badges.{alt,tooltip}.*", "badges.title.verified"));
        assert!(m(
            "categories.details.*.footer_text_html",
            "categories.details.roofing.footer_text_html"
        ));
        assert!(m(
            "cost_estimations_v3.categories.*.questions.*",
            "cost_estimations_v3.categories.x.questions.y.z"
        ));
        assert!(m("sort_ascending_by_*", "sort_ascending_by_price"));
        assert!(m("{refresh_quote, quote_now}", "quote_now"));
    }
}
