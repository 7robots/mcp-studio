//! A faithful port of the parts of Python's `difflib` the conformance store is
//! built from: `SequenceMatcher` (with `autojunk`, no `isjunk`) and
//! `unified_diff`.
//!
//! The blessed diffs were first written by a Python tool as
//! `"".join(difflib.unified_diff(a, b, fromfile, tofile, n=3))` over
//! `Path.read_text().splitlines(keepends=True)`. Reproducing that byte for byte
//! keeps every stored diff valid across the move, so the port follows CPython's
//! algorithm step by step, including its quirks: the 1% "popular element"
//! heuristic, no `\ No newline at end of file` marker, and `str.splitlines`'
//! full set of line boundaries.

use std::collections::HashMap;

/// `Path.read_text()`: universal newlines (`\r\n` and `\r` become `\n`).
pub fn universal_newlines(s: &str) -> String {
    if !s.contains('\r') {
        return s.to_string();
    }
    s.replace("\r\n", "\n").replace('\r', "\n")
}

fn is_line_boundary(c: char) -> bool {
    matches!(
        c,
        '\n' | '\r'
            | '\u{0b}'
            | '\u{0c}'
            | '\u{1c}'
            | '\u{1d}'
            | '\u{1e}'
            | '\u{85}'
            | '\u{2028}'
            | '\u{2029}'
    )
}

/// `str.splitlines(keepends=True)`.
pub fn splitlines_keepends(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut it = s.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        if is_line_boundary(c) {
            let mut end = i + c.len_utf8();
            if let (true, Some(&(j, '\n'))) = (c == '\r', it.peek()) {
                end = j + 1;
                it.next();
            }
            out.push(&s[start..end]);
            start = end;
        }
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// `str.splitlines()` (no line ends kept).
pub fn splitlines(s: &str) -> Vec<&str> {
    splitlines_keepends(s)
        .into_iter()
        .map(|l| {
            let mut end = l.len();
            if l.ends_with("\r\n") {
                end -= 2;
            } else if let Some(c) = l.chars().last().filter(|c| is_line_boundary(*c)) {
                end -= c.len_utf8();
            }
            &l[..end]
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Match {
    a: usize,
    b: usize,
    size: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tag {
    Replace,
    Delete,
    Insert,
    Equal,
}

type Opcode = (Tag, usize, usize, usize, usize);

struct SequenceMatcher<'a> {
    a: &'a [&'a str],
    b: &'a [&'a str],
    b2j: HashMap<&'a str, Vec<usize>>,
}

impl<'a> SequenceMatcher<'a> {
    fn new(a: &'a [&'a str], b: &'a [&'a str]) -> Self {
        let mut b2j: HashMap<&str, Vec<usize>> = HashMap::new();
        for (i, elt) in b.iter().enumerate() {
            b2j.entry(*elt).or_default().push(i);
        }
        // autojunk: purge elements more popular than 1% of a long b. No isjunk,
        // so nothing is junk and the junk-extension passes are no-ops.
        let n = b.len();
        if n >= 200 {
            let ntest = n / 100 + 1;
            b2j.retain(|_, idxs| idxs.len() <= ntest);
        }
        Self { a, b, b2j }
    }

    fn find_longest_match(&self, alo: usize, ahi: usize, blo: usize, bhi: usize) -> Match {
        let (a, b) = (self.a, self.b);
        let (mut besti, mut bestj, mut bestsize) = (alo, blo, 0usize);
        let mut j2len: HashMap<usize, usize> = HashMap::new();
        for (i, ai) in a.iter().enumerate().take(ahi).skip(alo) {
            let mut newj2len: HashMap<usize, usize> = HashMap::new();
            if let Some(js) = self.b2j.get(ai) {
                for &j in js {
                    if j < blo {
                        continue;
                    }
                    if j >= bhi {
                        break;
                    }
                    let k = j
                        .checked_sub(1)
                        .and_then(|p| j2len.get(&p))
                        .copied()
                        .unwrap_or(0)
                        + 1;
                    newj2len.insert(j, k);
                    if k > bestsize {
                        besti = i + 1 - k;
                        bestj = j + 1 - k;
                        bestsize = k;
                    }
                }
            }
            j2len = newj2len;
        }
        // Extend by equal (non-junk; nothing is junk here) elements each side.
        while besti > alo && bestj > blo && a[besti - 1] == b[bestj - 1] {
            besti -= 1;
            bestj -= 1;
            bestsize += 1;
        }
        while besti + bestsize < ahi
            && bestj + bestsize < bhi
            && a[besti + bestsize] == b[bestj + bestsize]
        {
            bestsize += 1;
        }
        Match {
            a: besti,
            b: bestj,
            size: bestsize,
        }
    }

    fn matching_blocks(&self) -> Vec<Match> {
        let (la, lb) = (self.a.len(), self.b.len());
        let mut queue = vec![(0, la, 0, lb)];
        let mut blocks = Vec::new();
        while let Some((alo, ahi, blo, bhi)) = queue.pop() {
            let m = self.find_longest_match(alo, ahi, blo, bhi);
            let (i, j, k) = (m.a, m.b, m.size);
            if k > 0 {
                blocks.push(m);
                if alo < i && blo < j {
                    queue.push((alo, i, blo, j));
                }
                if i + k < ahi && j + k < bhi {
                    queue.push((i + k, ahi, j + k, bhi));
                }
            }
        }
        blocks.sort();
        let (mut i1, mut j1, mut k1) = (0, 0, 0);
        let mut out = Vec::new();
        for m in blocks {
            if i1 + k1 == m.a && j1 + k1 == m.b {
                k1 += m.size;
            } else {
                if k1 > 0 {
                    out.push(Match {
                        a: i1,
                        b: j1,
                        size: k1,
                    });
                }
                (i1, j1, k1) = (m.a, m.b, m.size);
            }
        }
        if k1 > 0 {
            out.push(Match {
                a: i1,
                b: j1,
                size: k1,
            });
        }
        out.push(Match {
            a: la,
            b: lb,
            size: 0,
        });
        out
    }

    fn opcodes(&self) -> Vec<Opcode> {
        let (mut i, mut j) = (0, 0);
        let mut out = Vec::new();
        for m in self.matching_blocks() {
            let tag = if i < m.a && j < m.b {
                Some(Tag::Replace)
            } else if i < m.a {
                Some(Tag::Delete)
            } else if j < m.b {
                Some(Tag::Insert)
            } else {
                None
            };
            if let Some(t) = tag {
                out.push((t, i, m.a, j, m.b));
            }
            i = m.a + m.size;
            j = m.b + m.size;
            if m.size > 0 {
                out.push((Tag::Equal, m.a, i, m.b, j));
            }
        }
        out
    }

    fn grouped_opcodes(&self, n: usize) -> Vec<Vec<Opcode>> {
        let mut codes = self.opcodes();
        if codes.is_empty() {
            codes.push((Tag::Equal, 0, 1, 0, 1));
        }
        if let Some(first) = codes.first_mut().filter(|c| c.0 == Tag::Equal) {
            let (t, i1, i2, j1, j2) = *first;
            *first = (
                t,
                i1.max(i2.saturating_sub(n)),
                i2,
                j1.max(j2.saturating_sub(n)),
                j2,
            );
        }
        if let Some(last) = codes.last_mut().filter(|c| c.0 == Tag::Equal) {
            let (t, i1, i2, j1, j2) = *last;
            *last = (t, i1, i2.min(i1 + n), j1, j2.min(j1 + n));
        }
        let nn = n + n;
        let mut groups = Vec::new();
        let mut group = Vec::new();
        for (tag, mut i1, i2, mut j1, j2) in codes {
            if tag == Tag::Equal && i2 - i1 > nn {
                group.push((tag, i1, i2.min(i1 + n), j1, j2.min(j1 + n)));
                groups.push(std::mem::take(&mut group));
                i1 = i1.max(i2.saturating_sub(n));
                j1 = j1.max(j2.saturating_sub(n));
            }
            group.push((tag, i1, i2, j1, j2));
        }
        if !group.is_empty() && !(group.len() == 1 && group[0].0 == Tag::Equal) {
            groups.push(group);
        }
        groups
    }
}

fn format_range_unified(start: usize, stop: usize) -> String {
    let mut beginning = start + 1;
    let length = stop - start;
    if length == 1 {
        return beginning.to_string();
    }
    if length == 0 {
        beginning -= 1;
    }
    format!("{beginning},{length}")
}

/// `"".join(difflib.unified_diff(a, b, fromfile, tofile, n=n))` with no dates
/// and `lineterm="\n"`, where `a`/`b` are `splitlines(keepends=True)` lines.
pub fn unified_diff(a: &[&str], b: &[&str], fromfile: &str, tofile: &str, n: usize) -> String {
    let sm = SequenceMatcher::new(a, b);
    let mut out = String::new();
    for (gi, group) in sm.grouped_opcodes(n).iter().enumerate() {
        if gi == 0 {
            out.push_str(&format!("--- {fromfile}\n+++ {tofile}\n"));
        }
        let (first, last) = (group[0], group[group.len() - 1]);
        out.push_str(&format!(
            "@@ -{} +{} @@\n",
            format_range_unified(first.1, last.2),
            format_range_unified(first.3, last.4)
        ));
        for &(tag, i1, i2, j1, j2) in group {
            if tag == Tag::Equal {
                for l in &a[i1..i2] {
                    out.push(' ');
                    out.push_str(l);
                }
                continue;
            }
            if matches!(tag, Tag::Replace | Tag::Delete) {
                for l in &a[i1..i2] {
                    out.push('-');
                    out.push_str(l);
                }
            }
            if matches!(tag, Tag::Replace | Tag::Insert) {
                for l in &b[j1..j2] {
                    out.push('+');
                    out.push_str(l);
                }
            }
        }
    }
    out
}

/// Unified diff of two texts as the conformance tool computes it: universal
/// newlines, `splitlines(keepends=True)`, three lines of context.
pub fn diff_texts(a: &str, b: &str, fromfile: &str, tofile: &str) -> String {
    let (a, b) = (universal_newlines(a), universal_newlines(b));
    let al = splitlines_keepends(&a);
    let bl = splitlines_keepends(&b);
    unified_diff(&al, &bl, fromfile, tofile, 3)
}

/// Lines of a diff that differ (`+`/`-`, not the file headers), counted the
/// way the original status report did: over `str.splitlines()`, where an empty
/// line also counts (`"" in "+-"` is true in Python).
pub fn count_changed_lines(diff: &str) -> usize {
    splitlines(diff)
        .into_iter()
        .filter(|l| {
            let first_ok = l.is_empty() || l.starts_with('+') || l.starts_with('-');
            let head: String = l.chars().take(3).collect();
            first_ok && head != "+++" && head != "---"
        })
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(s: &str) -> Vec<&str> {
        splitlines_keepends(s)
    }

    #[test]
    fn identical_inputs_give_an_empty_diff() {
        let a = lines("x\ny\n");
        assert_eq!(unified_diff(&a, &a, "a", "b", 3), "");
        assert_eq!(unified_diff(&[], &[], "a", "b", 3), "");
    }

    #[test]
    fn matches_python_for_a_small_replace() {
        // python3 -c 'import difflib,sys;sys.stdout.write("".join(difflib.unified_diff(
        //   "a\nb\nc\nd\ne\nf\ng\nh\n".splitlines(True), "a\nb\nc\nD\ne\nf\ng\nh\ni\n".splitlines(True),
        //   fromfile="template/x", tofile="r/x")))'
        let a = lines("a\nb\nc\nd\ne\nf\ng\nh\n");
        let b = lines("a\nb\nc\nD\ne\nf\ng\nh\ni\n");
        assert_eq!(
            unified_diff(&a, &b, "template/x", "r/x", 3),
            "--- template/x\n+++ r/x\n@@ -1,8 +1,9 @@\n a\n b\n c\n-d\n+D\n e\n f\n g\n h\n+i\n"
        );
    }

    #[test]
    fn far_apart_changes_make_two_hunks() {
        let a: String = (0..20).map(|i| format!("{i}\n")).collect();
        let b: String = (0..20)
            .map(|i| match i {
                2 => "two\n".to_string(),
                17 => "seventeen\n".to_string(),
                _ => format!("{i}\n"),
            })
            .collect();
        assert_eq!(
            diff_texts(&a, &b, "t", "r"),
            "--- t\n+++ r\n@@ -1,6 +1,6 @@\n 0\n 1\n-2\n+two\n 3\n 4\n 5\n\
             @@ -15,6 +15,6 @@\n 14\n 15\n 16\n-17\n+seventeen\n 18\n 19\n"
        );
    }

    #[test]
    fn a_missing_side_is_all_inserts_with_an_empty_range() {
        let d = diff_texts("", "x\ny\n", "template/f", "r/f");
        assert_eq!(d, "--- template/f\n+++ r/f\n@@ -0,0 +1,2 @@\n+x\n+y\n");
    }

    #[test]
    fn no_newline_at_eof_is_not_marked() {
        let d = diff_texts("a\n", "a\nb", "t", "r");
        assert_eq!(d, "--- t\n+++ r\n@@ -1 +1,2 @@\n a\n+b");
    }

    #[test]
    fn splitlines_follows_python() {
        assert_eq!(
            splitlines_keepends("a\r\nb\rc\u{2028}d"),
            vec!["a\r\n", "b\r", "c\u{2028}", "d"]
        );
        assert_eq!(splitlines("a\nb\n"), vec!["a", "b"]);
        assert_eq!(universal_newlines("a\r\nb\rc"), "a\nb\nc");
    }

    #[test]
    fn changed_lines_skip_headers_and_count_dashes_like_python() {
        let d = "--- t\n+++ r\n@@ -1 +1 @@\n-x\n+y\n---z\n";
        // `---z` is a removed line "--z", which the original count skipped.
        assert_eq!(count_changed_lines(d), 2);
    }

    #[test]
    fn popular_lines_in_long_inputs_follow_autojunk() {
        // 300 lines, a blank line every third: blank lines are "popular" in b
        // and cannot anchor a match. Must not panic and must round-trip sanely.
        let a: String = (0..300)
            .map(|i| {
                if i % 3 == 0 {
                    "\n".to_string()
                } else {
                    format!("{i}\n")
                }
            })
            .collect();
        let b = a.replacen("151\n", "x\n", 1);
        let d = diff_texts(&a, &b, "t", "r");
        assert_eq!(
            d,
            "--- t\n+++ r\n@@ -149,7 +149,7 @@\n 148\n 149\n \n-151\n+x\n 152\n \n 154\n"
        );
        assert_eq!(count_changed_lines(&d), 2);
    }
}
