//! Calculations → instructions. Structured operations (IF, DOW, DOU,
//! FOR, SELECT, MONITOR...) become jumps, so GOTO, TAG and CABxx work
//! with them, as in the RPG cycle.

use super::ast::*;
use anyhow::{anyhow, bail, Result};
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq)]
pub enum Ins {
    /// Run a statement.
    Stmt(Stmt),
    Jump(usize),
    /// Jump when the condition is false.
    JumpIfNot(Expr, usize),
    /// FOR and DO: compare the index with the limit; jump to the end when
    /// it is past it (`down`: counting down).
    LoopTest {
        var: Expr,
        limit: Option<Expr>,
        down: bool,
        end: usize,
    },
    /// Add `by` (or subtract, counting down) to the index.
    LoopStep {
        var: Expr,
        by: Option<Expr>,
        down: bool,
    },
    /// FOR-EACH: take the list's values (once, as the loop starts).
    EachInit {
        list: Expr,
    },
    /// FOR-EACH: the next value into the item, or to `end` when there is
    /// none; `init` is the EachInit instruction.
    EachNext {
        var: Expr,
        init: usize,
        end: usize,
    },
    ExSr(String),
    Return(Option<Expr>),
    LeaveSr,
    Cab {
        cmp: Option<BinOp>,
        f1: Expr,
        f2: Expr,
        to: usize,
        ind: ResultInd,
        /// The TAG is outside this subroutine (in the calculations it
        /// was run from).
        out: Option<String>,
    },
    /// TAG: where a GOTO from a subroutine lands.
    Label(String),
    /// GOTO from a subroutine to a TAG in the calculations it was run
    /// from (detail or total calculations, or the procedure's body).
    GotoOut(String),
    /// ENDSR with a return point.
    EndSr(Expr),
    /// MONITOR: errors in the instructions up to `end_body` go to the
    /// ON-ERROR handlers.
    Monitor {
        handlers: Vec<(Vec<String>, usize)>,
        end_body: usize,
        end: usize,
    },
    /// The end of a monitored body or of an ON-ERROR handler.
    EndMonitor {
        end: usize,
    },
    Nop,
}

/// One instruction with its source line and conditioning indicators.
#[derive(Clone, Debug, PartialEq)]
pub struct I {
    pub ins: Ins,
    pub line: usize,
    pub cond: Conditions,
}

pub type Code = Vec<I>;

/// A routine's calculations, split: detail calculations, total
/// calculations (L1-L9, LR in positions 7-8), subroutines.
#[derive(Clone, Debug, Default)]
pub struct Routines {
    pub detail: Code,
    pub total: Code,
    pub subroutines: HashMap<String, Code>,
}

#[allow(clippy::large_enum_variant)]
enum Open {
    If {
        next: Option<usize>,
        ends: Vec<usize>,
    },
    Select {
        next: Option<usize>,
        ends: Vec<usize>,
        other: bool,
    },
    Loop {
        kind: LoopKind,
        start: usize,
        iters: Vec<usize>,
        leaves: Vec<usize>,
        cond: Option<Expr>,
        var: Option<(Expr, Option<Expr>, bool)>,
    },
    Monitor {
        at: usize,
        handlers: Vec<(Vec<String>, usize)>,
        ends: Vec<usize>,
    },
}

#[derive(Clone, Copy, PartialEq)]
enum LoopKind {
    Dow,
    Dou,
    For,
    Each,
}

struct Builder {
    code: Code,
    stack: Vec<Open>,
    tags: HashMap<String, usize>,
    gotos: Vec<(usize, String)>,
}

impl Builder {
    fn new() -> Builder {
        Builder {
            code: vec![],
            stack: vec![],
            tags: HashMap::new(),
            gotos: vec![],
        }
    }

    fn here(&self) -> usize {
        self.code.len()
    }

    fn push(&mut self, ins: Ins, line: usize, cond: Conditions) -> usize {
        self.code.push(I { ins, line, cond });
        self.code.len() - 1
    }

    fn patch(&mut self, at: usize, to: usize) {
        match &mut self.code[at].ins {
            Ins::Jump(t) | Ins::JumpIfNot(_, t) => *t = to,
            Ins::LoopTest { end, .. } => *end = to,
            Ins::Cab { to: t, .. } => *t = to,
            Ins::EachNext { end, .. } => *end = to,
            _ => {}
        }
    }

    fn end_loop(&mut self, line: usize, what: &str) -> Result<()> {
        let Some(Open::Loop {
            kind,
            start,
            iters,
            leaves,
            cond,
            var,
        }) = self.stack.pop()
        else {
            bail!("line {line}: {what} without DO, DOW, DOU or FOR");
        };
        let cont = self.here();
        match kind {
            LoopKind::Dou => {
                let c = cond.ok_or_else(|| anyhow!("DOU without a condition"))?;
                self.push(Ins::JumpIfNot(c, start), line, vec![]);
            }
            LoopKind::Dow | LoopKind::Each => {
                self.push(Ins::Jump(start), line, vec![]);
            }
            LoopKind::For => {
                let (v, by, down) = var.ok_or_else(|| anyhow!("FOR without an index"))?;
                self.push(Ins::LoopStep { var: v, by, down }, line, vec![]);
                self.push(Ins::Jump(start), line, vec![]);
            }
        }
        let end = self.here();
        for i in iters {
            self.patch(i, cont);
        }
        for l in leaves {
            self.patch(l, end);
        }
        // DOW and FOR test at the start: that test jumps to the end.
        if kind != LoopKind::Dou {
            self.patch(start, end);
        }
        Ok(())
    }

    fn line(&mut self, cl: &CalcLine) -> Result<()> {
        let line = cl.line;
        let cond = cl.cond.clone();
        match &cl.what {
            Line::Stmt(s) => {
                self.push(Ins::Stmt(s.clone()), line, cond);
            }
            Line::If(c) => {
                let at = self.push(Ins::JumpIfNot(c.clone(), 0), line, vec![]);
                self.stack.push(Open::If {
                    next: Some(at),
                    ends: vec![],
                });
            }
            Line::ElseIf(c) => {
                let j = self.push(Ins::Jump(0), line, vec![]);
                let here = self.here();
                match self.stack.last_mut() {
                    Some(Open::If { next, ends }) => {
                        ends.push(j);
                        if let Some(n) = next.take() {
                            self.code[n].ins =
                                match std::mem::replace(&mut self.code[n].ins, Ins::Nop) {
                                    Ins::JumpIfNot(e, _) => Ins::JumpIfNot(e, here),
                                    other => other,
                                };
                        }
                    }
                    _ => bail!("line {line}: ELSEIF without IF"),
                }
                let at = self.push(Ins::JumpIfNot(c.clone(), 0), line, vec![]);
                if let Some(Open::If { next, .. }) = self.stack.last_mut() {
                    *next = Some(at);
                }
            }
            Line::Else => {
                let j = self.push(Ins::Jump(0), line, vec![]);
                let here = self.here();
                let Some(Open::If { next, ends }) = self.stack.last_mut() else {
                    bail!("line {line}: ELSE without IF");
                };
                ends.push(j);
                let n = next.take();
                if let Some(n) = n {
                    self.patch(n, here);
                }
            }
            Line::EndIf => self.end_if(line)?,
            Line::Dow(c) => {
                let at = self.push(Ins::JumpIfNot(c.clone(), 0), line, vec![]);
                self.stack.push(Open::Loop {
                    kind: LoopKind::Dow,
                    start: at,
                    iters: vec![],
                    leaves: vec![],
                    cond: None,
                    var: None,
                });
            }
            Line::Dou(c) => {
                let start = self.here();
                self.stack.push(Open::Loop {
                    kind: LoopKind::Dou,
                    start,
                    iters: vec![],
                    leaves: vec![],
                    cond: Some(c.clone()),
                    var: None,
                });
            }
            Line::For {
                var,
                from,
                to,
                by,
                down,
            } => {
                self.push(
                    Ins::Stmt(Stmt::Eval {
                        target: var.clone(),
                        value: from.clone(),
                        ext: Ext::default(),
                        right: false,
                    }),
                    line,
                    vec![],
                );
                let at = self.push(
                    Ins::LoopTest {
                        var: var.clone(),
                        limit: to.clone(),
                        down: *down,
                        end: 0,
                    },
                    line,
                    vec![],
                );
                self.stack.push(Open::Loop {
                    kind: LoopKind::For,
                    start: at,
                    iters: vec![],
                    leaves: vec![],
                    cond: None,
                    var: Some((var.clone(), by.clone(), *down)),
                });
            }
            Line::Do {
                start,
                limit,
                index,
            } => {
                // DO: the index starts at factor 1 (default 1) and goes up
                // to factor 2 (default 1).
                let var = index
                    .clone()
                    .unwrap_or_else(|| Expr::name(&format!("*DO{}", self.here())));
                let one = Expr::Num(super::dec::Dec::from_i64(1));
                self.push(
                    Ins::Stmt(Stmt::Eval {
                        target: var.clone(),
                        value: start.clone().unwrap_or(one.clone()),
                        ext: Ext::default(),
                        right: false,
                    }),
                    line,
                    vec![],
                );
                let at = self.push(
                    Ins::LoopTest {
                        var: var.clone(),
                        limit: Some(limit.clone().unwrap_or(one)),
                        down: false,
                        end: 0,
                    },
                    line,
                    vec![],
                );
                self.stack.push(Open::Loop {
                    kind: LoopKind::For,
                    start: at,
                    iters: vec![],
                    leaves: vec![],
                    cond: None,
                    var: Some((var, None, false)),
                });
            }
            Line::ForEach { var, list } => {
                let init = self.push(Ins::EachInit { list: list.clone() }, line, vec![]);
                let at = self.push(
                    Ins::EachNext {
                        var: var.clone(),
                        init,
                        end: 0,
                    },
                    line,
                    vec![],
                );
                self.stack.push(Open::Loop {
                    kind: LoopKind::Each,
                    start: at,
                    iters: vec![],
                    leaves: vec![],
                    cond: None,
                    var: None,
                });
            }
            Line::EndDo | Line::EndFor => self.end_loop(line, "ENDDO")?,
            Line::Select => self.stack.push(Open::Select {
                next: None,
                ends: vec![],
                other: false,
            }),
            Line::When(c) => {
                let mut pending = None;
                if let Some(Open::Select { next, .. }) = self.stack.last_mut() {
                    pending = Some(next.take());
                }
                let Some(prev_next) = pending else {
                    bail!("line {line}: WHEN without SELECT")
                };
                if let Some(n) = prev_next {
                    let j = self.push(Ins::Jump(0), line, vec![]);
                    let here = self.here();
                    self.patch(n, here);
                    if let Some(Open::Select { ends, .. }) = self.stack.last_mut() {
                        ends.push(j);
                    }
                }
                let at = self.push(Ins::JumpIfNot(c.clone(), 0), line, vec![]);
                if let Some(Open::Select { next, .. }) = self.stack.last_mut() {
                    *next = Some(at);
                }
            }
            Line::Other => {
                let n = match self.stack.last_mut() {
                    Some(Open::Select { next, other, .. }) => {
                        *other = true;
                        next.take()
                    }
                    _ => bail!("line {line}: OTHER without SELECT"),
                };
                if let Some(n) = n {
                    let j = self.push(Ins::Jump(0), line, vec![]);
                    let here = self.here();
                    self.patch(n, here);
                    if let Some(Open::Select { ends, .. }) = self.stack.last_mut() {
                        ends.push(j);
                    }
                }
            }
            Line::EndSl => self.end_select(line)?,
            Line::End => match self.stack.last() {
                Some(Open::If { .. }) => self.end_if(line)?,
                Some(Open::Loop { .. }) => self.end_loop(line, "END")?,
                Some(Open::Select { .. }) => self.end_select(line)?,
                _ => bail!("line {line}: END without an open IF, DO or SELECT"),
            },
            Line::Iter => {
                let j = self.push(Ins::Jump(0), line, cond);
                match self
                    .stack
                    .iter_mut()
                    .rev()
                    .find(|o| matches!(o, Open::Loop { .. }))
                {
                    Some(Open::Loop { iters, .. }) => iters.push(j),
                    _ => bail!("line {line}: ITER outside a loop"),
                }
            }
            Line::Leave => {
                let j = self.push(Ins::Jump(0), line, cond);
                match self
                    .stack
                    .iter_mut()
                    .rev()
                    .find(|o| matches!(o, Open::Loop { .. }))
                {
                    Some(Open::Loop { leaves, .. }) => leaves.push(j),
                    _ => bail!("line {line}: LEAVE outside a loop"),
                }
            }
            Line::LeaveSr => {
                self.push(Ins::LeaveSr, line, cond);
            }
            Line::ExSr(n) => {
                self.push(Ins::ExSr(n.clone()), line, cond);
            }
            Line::Return(e) => {
                self.push(Ins::Return(e.clone()), line, cond);
            }
            Line::Goto(t) => {
                let j = self.push(Ins::Jump(0), line, cond);
                self.gotos.push((j, t.clone()));
            }
            Line::Tag(t) => {
                let here = self.push(Ins::Label(t.clone()), line, vec![]);
                self.tags.insert(t.clone(), here);
            }
            Line::Cab {
                cmp,
                f1,
                f2,
                tag,
                ind,
            } => {
                let j = self.push(
                    Ins::Cab {
                        cmp: *cmp,
                        f1: f1.clone(),
                        f2: f2.clone(),
                        to: usize::MAX,
                        ind: ind.clone(),
                        out: None,
                    },
                    line,
                    cond,
                );
                if !tag.is_empty() {
                    self.gotos.push((j, tag.clone()));
                }
            }
            Line::Monitor => {
                let at = self.push(
                    Ins::Monitor {
                        handlers: vec![],
                        end_body: 0,
                        end: 0,
                    },
                    line,
                    vec![],
                );
                self.stack.push(Open::Monitor {
                    at,
                    handlers: vec![],
                    ends: vec![],
                });
            }
            Line::OnError(codes) => {
                let j = self.push(Ins::EndMonitor { end: 0 }, line, vec![]);
                let here = self.here();
                let Some(Open::Monitor { at, handlers, .. }) = self.stack.last_mut() else {
                    bail!("line {line}: ON-ERROR without MONITOR");
                };
                if handlers.is_empty() {
                    let at = *at;
                    if let Ins::Monitor { end_body, .. } = &mut self.code[at].ins {
                        *end_body = j;
                    }
                }
                if let Some(Open::Monitor { handlers, ends, .. }) = self.stack.last_mut() {
                    ends.push(j);
                    handlers.push((codes.clone(), here));
                }
            }
            Line::EndMon => {
                let j = self.push(Ins::EndMonitor { end: 0 }, line, vec![]);
                let end = self.here();
                let Some(Open::Monitor {
                    at,
                    handlers,
                    mut ends,
                }) = self.stack.pop()
                else {
                    bail!("line {line}: ENDMON without MONITOR");
                };
                ends.push(j);
                for e in ends {
                    if let Ins::EndMonitor { end: t } = &mut self.code[e].ins {
                        *t = end;
                    }
                }
                if let Ins::Monitor {
                    handlers: h,
                    end_body,
                    end: t,
                } = &mut self.code[at].ins
                {
                    if *end_body == 0 {
                        *end_body = j;
                    }
                    *h = handlers;
                    *t = end;
                }
            }
            Line::BegSr(_) | Line::EndSr(_) => unreachable!("subroutines are split before"),
        }
        Ok(())
    }

    fn end_if(&mut self, line: usize) -> Result<()> {
        let Some(Open::If { next, ends }) = self.stack.pop() else {
            bail!("line {line}: ENDIF without IF");
        };
        let here = self.here();
        if let Some(n) = next {
            self.patch(n, here);
        }
        for e in ends {
            self.patch(e, here);
        }
        Ok(())
    }

    fn end_select(&mut self, line: usize) -> Result<()> {
        let Some(Open::Select { next, ends, .. }) = self.stack.pop() else {
            bail!("line {line}: ENDSL without SELECT");
        };
        let here = self.here();
        if let Some(n) = next {
            self.patch(n, here);
        }
        for e in ends {
            self.patch(e, here);
        }
        Ok(())
    }

    fn finish(self, what: &str) -> Result<Code> {
        self.finish_with(what, false)
    }

    /// A subroutine's code: a GOTO to a TAG outside it goes to the
    /// calculations it was run from (ILE RPG Reference, GOTO: from a
    /// subroutine to the detail or total calculations).
    fn finish_sub(self, what: &str) -> Result<Code> {
        self.finish_with(what, true)
    }

    fn finish_with(mut self, what: &str, sub: bool) -> Result<Code> {
        if let Some(o) = self.stack.last() {
            let kind = match o {
                Open::If { .. } => "IF",
                Open::Select { .. } => "SELECT",
                Open::Loop { .. } => "DO/DOW/DOU/FOR",
                Open::Monitor { .. } => "MONITOR",
            };
            bail!("{what}: an {kind} has no end");
        }
        for (at, tag) in std::mem::take(&mut self.gotos) {
            match self.tags.get(&tag) {
                Some(&to) => self.patch(at, to),
                None if sub => match &mut self.code[at].ins {
                    Ins::Cab { out, .. } => *out = Some(tag),
                    other => *other = Ins::GotoOut(tag),
                },
                None => bail!("{what}: TAG {tag} not found"),
            }
        }
        Ok(self.code)
    }
}

/// Build the routines of a main procedure or subprocedure.
pub fn build(calcs: &[CalcLine]) -> Result<Routines> {
    let mut r = Routines::default();
    let mut detail = Builder::new();
    let mut total = Builder::new();
    let mut sub: Option<(String, Builder)> = None;
    // Total calculations run when their control level indicator is on:
    // each run of lines with the same level is one group (L0: every
    // total time).
    let mut open_level: Option<(String, usize)> = None;
    let close = |total: &mut Builder, open: &mut Option<(String, usize)>| -> Result<()> {
        if let Some((l, line)) = open.take() {
            if l != "L0" {
                total.line(&CalcLine {
                    line,
                    cond: vec![],
                    level: None,
                    what: Line::EndIf,
                })?;
            }
        }
        Ok(())
    };
    for cl in calcs {
        match &cl.what {
            Line::BegSr(n) => {
                if sub.is_some() {
                    bail!("line {}: BEGSR {n} inside another subroutine", cl.line);
                }
                close(&mut total, &mut open_level)?;
                sub = Some((n.clone(), Builder::new()));
                continue;
            }
            Line::EndSr(point) => {
                let (n, mut b) = sub
                    .take()
                    .ok_or_else(|| anyhow!("line {}: ENDSR without BEGSR", cl.line))?;
                if let Some(p) = point {
                    b.push(Ins::EndSr(p.clone()), cl.line, vec![]);
                }
                let code = b.finish_sub(&format!("subroutine {n}"))?;
                r.subroutines.insert(n, code);
                continue;
            }
            _ => {}
        }
        let total_level = cl.level.as_deref().filter(|l| {
            *l == "LR" || (l.starts_with('L') && l[1..].chars().all(|c| c.is_ascii_digit()))
        });
        match (&mut sub, total_level) {
            (Some((_, b)), _) => b.line(cl)?,
            (None, Some(l)) => {
                if open_level.as_ref().map(|o| o.0.as_str()) != Some(l) {
                    close(&mut total, &mut open_level)?;
                    if l != "L0" {
                        total.line(&CalcLine {
                            line: cl.line,
                            cond: vec![],
                            level: None,
                            what: Line::If(Expr::Ind(l.to_string())),
                        })?;
                    }
                    open_level = Some((l.to_string(), cl.line));
                }
                total.line(cl)?;
            }
            (None, None) => {
                // A detail line ends a group of total lines written
                // between detail calculations.
                detail.line(cl)?;
            }
        }
    }
    close(&mut total, &mut open_level)?;
    if let Some((n, _)) = sub {
        bail!("subroutine {n} has no ENDSR");
    }
    r.detail = detail.finish("calculations")?;
    r.total = total.finish("total calculations")?;
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpg::parse;

    #[test]
    fn blocks_become_jumps() {
        let src = parse::parse("**FREE\nif a = 1;\n x = 1;\nelseif a = 2;\n x = 2;\nelse;\n x = 3;\nendif;\ndow b < 3;\n b += 1;\n if b = 2;\n  iter;\n endif;\nenddo;\nbegsr s1;\n x = 9;\nendsr;\n", &|_| None).unwrap();
        let r = build(&src.calcs).unwrap();
        assert!(r.subroutines.contains_key("S1"));
        // Every jump goes somewhere inside the code (or just past it).
        for i in &r.detail {
            if let Ins::Jump(t) | Ins::JumpIfNot(_, t) = &i.ins {
                assert!(*t <= r.detail.len(), "{:?}", r.detail);
            }
        }
        assert!(build(&parse::parse("**FREE\nif a;\n", &|_| None).unwrap().calcs).is_err());
    }
}
