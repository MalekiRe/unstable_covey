//! A system's machine code, on demand: every instruction whose inline chain
//! passes through the system's body, from a binary built with line tables
//! (`CARGO_PROFILE_RELEASE_DEBUG=line-tables-only`, which leaves code
//! generation unchanged). Systems are inlined into their frame, so there is
//! no symbol to disassemble; the line tables say which instructions came
//! from where. Uses GNU `objdump`.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};

/// Which code to collect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// A function by path (`game::apply_damage`); matched by its last
    /// segment in the line tables' function names.
    Function(String),
    /// Source lines `first..=last` of a file (a closure's body). `last` is
    /// found from the source, read where the line tables say it lives.
    Lines {
        file: String,
        first: u32,
        last: Option<u32>,
    },
}
impl Target {
    /// `path::to::fn`, or `file.rs:LINE` for the brace block starting there
    /// (a closure registered at that line).
    pub fn parse(spec: &str) -> Result<Target, String> {
        if let Some((file, line)) = spec.rsplit_once(':')
            && let Ok(first) = line.parse::<u32>()
            && file.ends_with(".rs")
        {
            return Ok(Target::Lines {
                file: file.to_string(),
                first,
                last: block_end(file, first),
            });
        }
        let name = spec.trim_end_matches("::{{closure}}");
        let short = name.rsplit("::").next().unwrap_or(name);
        if short.is_empty() {
            return Err(format!("`{spec}` names no function"));
        }
        Ok(Target::Function(short.to_string()))
    }
    fn matches(&mut self, frame: &Frame) -> bool {
        match self {
            Target::Function(name) => {
                let short = |f: &str| {
                    let f = f.split('<').next().unwrap_or(f);
                    f.rsplit("::").next().unwrap_or(f).to_string()
                };
                frame.function.as_deref().is_some_and(|f| short(f) == *name)
            }
            Target::Lines { file, first, last } => {
                let same =
                    frame.file.ends_with(file.as_str()) || file.ends_with(frame.file.as_str());
                if same && last.is_none() {
                    // The line tables hold the absolute path: read it there.
                    *last = Some(block_end(&frame.file, *first).unwrap_or(*first));
                }
                same && (*first..=last.unwrap_or(*first)).contains(&frame.line)
            }
        }
    }
}

/// The last line of the brace block that starts on or after `first`.
fn block_end(file: &str, first: u32) -> Option<u32> {
    let source = std::fs::read_to_string(file).ok()?;
    let mut depth = 0i32;
    let mut opened = false;
    for (number, line) in source.lines().enumerate().skip(first as usize - 1) {
        for c in line.chars() {
            match c {
                '{' => {
                    depth += 1;
                    opened = true;
                }
                '}' => depth -= 1,
                _ => {}
            }
            if opened && depth == 0 {
                return Some(number as u32 + 1);
            }
        }
    }
    None
}

/// One source location in an instruction's inline chain.
#[derive(Clone, Debug)]
struct Frame {
    file: String,
    line: u32,
    function: Option<String>,
}

/// A system's instructions.
#[derive(Clone, Debug, Default)]
pub struct Code {
    /// Normalized instructions (addresses and offsets elided), in address order.
    pub instructions: Vec<String>,
    /// The symbols the code was inlined into.
    pub symbols: BTreeSet<String>,
}
impl Code {
    /// Counts that say most about codegen.
    pub fn stats(&self) -> String {
        let count =
            |test: &dyn Fn(&str) -> bool| self.instructions.iter().filter(|i| test(i)).count();
        let vector = count(&|i| {
            i.contains("%ymm") || i.contains("%zmm") || (i.contains("%xmm") && i.contains('p'))
        });
        format!(
            "{} instructions ({} vector, {} calls, {} branches) in {}",
            self.instructions.len(),
            vector,
            count(&|i| i.starts_with("call")),
            count(&|i| i.starts_with('j')),
            self.symbols
                .iter()
                .map(|s| super::value::short_type(s))
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
    /// Stats, then the instructions.
    pub fn text(&self) -> String {
        let mut out = self.stats();
        for instruction in &self.instructions {
            out.push_str("\n  ");
            out.push_str(instruction);
        }
        out
    }
}

/// Elide what moves between builds: absolute addresses and their symbol
/// offsets, and rip-relative displacements.
fn normalize(instruction: &str) -> String {
    let mut out = String::with_capacity(instruction.len());
    let mut words = instruction.split_whitespace().peekable();
    while let Some(word) = words.next() {
        if !out.is_empty() {
            out.push(' ');
        }
        let hex = word.trim_end_matches(',');
        if hex.len() >= 4
            && hex.chars().all(|c| c.is_ascii_hexdigit())
            && words.peek().is_some_and(|w| w.starts_with('<'))
        {
            out.push_str("<addr>");
            // The `<symbol+0x..>` that follows.
            if let Some(symbol) = words.next() {
                out.push(' ');
                out.push_str(
                    symbol
                        .split('+')
                        .next()
                        .unwrap_or(symbol)
                        .trim_end_matches('>'),
                );
                out.push('>');
            }
            continue;
        }
        if let Some(at) = word.find("(%rip)") {
            out.push_str("<rip>");
            out.push_str(&word[at + 6..]);
            continue;
        }
        if let Some(comment) = word.strip_prefix('#') {
            let _ = comment;
            break;
        }
        out.push_str(word);
    }
    out
}

/// The instructions of `target` in `binary`.
pub fn collect(binary: &Path, target: &Target) -> Result<Code, String> {
    let mut child = Command::new("objdump")
        .args([
            "-d",
            "-l",
            "--inlines",
            "--no-show-raw-insn",
            "-C",
            "--no-addresses",
        ])
        .arg(binary)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("objdump: {e} (install GNU binutils)"))?;
    let stdout = child.stdout.take().ok_or("objdump produced no output")?;
    let lines = BufReader::new(stdout).lines().map_while(Result::ok);
    let (code, saw_lines) = scan(lines, &mut target.clone());
    let _ = child.wait();
    if !saw_lines {
        return Err(format!(
            "{} has no line tables: rebuild with CARGO_PROFILE_RELEASE_DEBUG=line-tables-only",
            binary.display()
        ));
    }
    if code.instructions.is_empty() {
        return Err(format!(
            "no instructions from {target:?}: wrong name or lines, or it was optimized away"
        ));
    }
    Ok(code)
}

/// Collect `target`'s instructions from `objdump -d -l --inlines
/// --no-addresses` output; also whether it had line tables at all. Each
/// location block (innermost line, then `inlined by` callers) applies to the
/// instructions after it; an instruction belongs to the target when any
/// frame of its chain does.
pub fn scan(lines: impl Iterator<Item = String>, target: &mut Target) -> (Code, bool) {
    let mut code = Code::default();
    let mut symbol = String::new();
    let mut function: Option<String> = None;
    let mut matched = false;
    let mut saw_lines = false;
    for line in lines {
        if let Some(name) = line.strip_prefix('<').and_then(|l| l.strip_suffix(">:")) {
            symbol = name.to_string();
            matched = false;
        } else if let Some(rest) = line.strip_prefix("inlined by ") {
            matched |= parse_frame(rest).is_some_and(|frame| target.matches(&frame));
        } else if line.starts_with('/') || line.starts_with("./") {
            if let Some(mut frame) = parse_frame(&line) {
                saw_lines = true;
                frame.function = function.clone();
                matched = target.matches(&frame);
            }
        } else if let Some(name) = line
            .strip_suffix(':')
            .filter(|l| !l.starts_with(char::is_whitespace) && !l.is_empty())
        {
            function = Some(name.trim_end_matches("()").to_string());
        } else if line.starts_with(char::is_whitespace) && matched {
            let instruction = line.trim();
            if !instruction.is_empty() {
                code.instructions.push(normalize(instruction));
                code.symbols.insert(symbol.clone());
            }
        }
    }
    (code, saw_lines)
}

/// `file:line (function)` or `file:line (discriminator N)`.
fn parse_frame(text: &str) -> Option<Frame> {
    let (location, rest) = match text.find(" (") {
        Some(at) => (&text[..at], Some(&text[at + 2..])),
        None => (text, None),
    };
    let (file, line) = location.rsplit_once(':')?;
    let function = rest
        .map(|r| r.trim_end_matches(')'))
        .filter(|r| !r.starts_with("discriminator"))
        .map(str::to_string);
    Some(Frame {
        file: file.to_string(),
        line: line.parse().ok()?,
        function,
    })
}

/// A system's code: `spec` as for [`Target::parse`].
pub fn system(binary: &Path, spec: &str) -> Result<Code, String> {
    collect(binary, &Target::parse(spec)?)
}

/// Unified diff of a system's code between two builds, with stats.
pub fn diff(before: &Path, after: &Path, spec: &str) -> Result<String, String> {
    let target = Target::parse(spec)?;
    let (a, b) = (collect(before, &target)?, collect(after, &target)?);
    let mut out = format!("before: {}\nafter:  {}\n", a.stats(), b.stats());
    if a.instructions == b.instructions {
        out.push_str("identical instructions");
        return Ok(out);
    }
    out.push_str(&unified(&a.instructions, &b.instructions));
    Ok(out)
}

/// A line diff (longest common subsequence), with three lines of context.
fn unified(a: &[String], b: &[String]) -> String {
    const LIMIT: usize = 6000;
    if a.len() > LIMIT || b.len() > LIMIT {
        return format!(
            "too large to diff line by line ({} vs {} instructions)",
            a.len(),
            b.len()
        );
    }
    let (n, m) = (a.len(), b.len());
    let mut table = vec![0u32; (n + 1) * (m + 1)];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[i * (m + 1) + j] = if a[i] == b[j] {
                table[(i + 1) * (m + 1) + j + 1] + 1
            } else {
                table[(i + 1) * (m + 1) + j].max(table[i * (m + 1) + j + 1])
            };
        }
    }
    let mut ops = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < n || j < m {
        if i < n && j < m && a[i] == b[j] {
            ops.push((' ', &a[i]));
            i += 1;
            j += 1;
        } else if j < m && (i == n || table[i * (m + 1) + j + 1] >= table[(i + 1) * (m + 1) + j]) {
            ops.push(('+', &b[j]));
            j += 1;
        } else {
            ops.push(('-', &a[i]));
            i += 1;
        }
    }
    let changed: Vec<bool> = ops.iter().map(|(op, _)| *op != ' ').collect();
    let near = |k: usize| (k.saturating_sub(3)..(k + 4).min(ops.len())).any(|x| changed[x]);
    let mut out = String::new();
    let mut gap = false;
    for (k, (op, line)) in ops.iter().enumerate() {
        if near(k) {
            if gap {
                out.push_str("  …\n");
                gap = false;
            }
            let _ = writeln!(out, "{op} {line}");
        } else {
            gap = true;
        }
    }
    out
}
