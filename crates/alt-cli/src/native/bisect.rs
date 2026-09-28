//! `alt bisect`: binary search for the commit that introduced a change, as
//! git does. While it runs, the workspace HEAD points at a scratch ref on the
//! commit being tested; `reset` puts HEAD back on the branch.

use super::*;

/// What a bisect session knows so far.
#[derive(Default)]
struct BisectState {
    /// The branch HEAD was on when the session started.
    branch: String,
    bad: Option<ObjectId>,
    good: Vec<ObjectId>,
    skip: Vec<ObjectId>,
}

/// Where the search stands after the latest verdict.
enum Next {
    /// Test this commit next; how many remain afterwards, roughly how many
    /// steps.
    Test(ObjectId, usize, u32),
    /// Only this commit can be the first bad one.
    Found(ObjectId),
    /// The first bad commit is among these, but they were all skipped.
    OnlySkipped(Vec<ObjectId>),
    /// Still waiting for a good and a bad commit.
    Waiting,
}

impl NativeRepo<'_> {
    /// `alt bisect start [<bad> [<good>...]]`.
    pub fn bisect_start(
        &mut self,
        bad: Option<&str>,
        good: &[String],
        out: &mut impl Write,
    ) -> Res<()> {
        self.ensure_no_merge_in_progress("bisect start")?;
        self.ensure_clean("bisect start")?;
        let branch = self.head_branch()?;
        if !branch.starts_with("refs/heads/") {
            return Err("bisect start: HEAD is not on a branch".into());
        }
        let tip = self
            .store
            .refs
            .resolve(&branch)?
            .ok_or("bisect start: the branch has no commits yet")?;
        let mut state = BisectState {
            branch: branch.clone(),
            ..Default::default()
        };
        state.bad = bad.map(|r| self.rev(r)).transpose()?;
        for g in good {
            state.good.push(self.rev(g)?);
        }

        let scratch = self.bisect_ref();
        let head_ref = self.head_ref.clone();
        self.commit_refs(
            "bisect",
            &[
                RefChange {
                    name: scratch.clone(),
                    old: None,
                    new: Some(RefTarget::Oid(tip)),
                },
                RefChange {
                    name: head_ref,
                    old: Some(RefTarget::Symbolic(branch)),
                    new: Some(RefTarget::Symbolic(scratch)),
                },
            ],
        )?;
        self.save_bisect_state(&state)?;
        self.bisect_step(&state, out)?;
        Ok(())
    }

    /// `alt bisect good|bad|skip [<rev>...]`: record a verdict (on the
    /// commit being tested when no revision is given) and move on.
    pub fn bisect_mark(&mut self, verdict: &str, revs: &[String], out: &mut impl Write) -> Res<()> {
        self.mark(verdict, revs, out)?;
        Ok(())
    }

    fn mark(&mut self, verdict: &str, revs: &[String], out: &mut impl Write) -> Res<Next> {
        let mut state = self.bisect_state()?.ok_or_else(|| {
            format!("bisect {verdict}: no bisect in progress; run `alt bisect start`")
        })?;
        let commits = if revs.is_empty() {
            vec![self.bisect_head()?]
        } else {
            revs.iter().map(|r| self.rev(r)).collect::<Res<Vec<_>>>()?
        };
        match verdict {
            "bad" => {
                if commits.len() > 1 {
                    return Err("bisect bad: only one commit can be bad".into());
                }
                state.bad = Some(commits[0]);
            }
            "good" => state.good.extend(commits),
            "skip" => state.skip.extend(commits),
            _ => unreachable!("clap limits the verdicts"),
        }
        self.save_bisect_state(&state)?;
        self.bisect_step(&state, out)
    }

    /// `alt bisect reset`: end the session and put HEAD back on the branch.
    pub fn bisect_reset(&mut self, out: &mut impl Write) -> Res<()> {
        let state = self
            .bisect_state()?
            .ok_or("bisect reset: no bisect in progress")?;
        let at = self.bisect_head()?;
        self.ensure_clean("bisect reset")?;
        let tip = self
            .store
            .refs
            .resolve(&state.branch)?
            .ok_or("bisect reset: the branch is gone")?;
        let old = self.commit_entries(at)?;
        let target = self.commit_entries(tip)?;
        self.checkout(&old, &target)?;
        let scratch = self.bisect_ref();
        let head_ref = self.head_ref.clone();
        self.commit_refs(
            "bisect",
            &[
                RefChange {
                    name: head_ref,
                    old: Some(RefTarget::Symbolic(scratch.clone())),
                    new: Some(RefTarget::Symbolic(state.branch.clone())),
                },
                RefChange {
                    name: scratch,
                    old: Some(RefTarget::Oid(at)),
                    new: None,
                },
            ],
        )?;
        std::fs::remove_file(self.bisect_state_path())?;
        let short = state
            .branch
            .strip_prefix("refs/heads/")
            .unwrap_or(&state.branch);
        writeln!(out, "Switched to branch '{short}'")?;
        Ok(())
    }

    /// `alt bisect run <cmd> [<arg>...]`: let a command decide each step, with
    /// git's exit-code rules: 0 good, 125 skip, 1–127 bad, 128 or more stops.
    pub fn bisect_run(&mut self, cmd: &[String], out: &mut impl Write) -> Res<()> {
        if self.bisect_state()?.is_none() {
            return Err("bisect run: no bisect in progress; run `alt bisect start`".into());
        }
        let script = format!("{} \"$@\"", cmd[0]);
        loop {
            let at = self.bisect_head()?;
            writeln!(out, "running {}", cmd.join(" "))?;
            let status = std::process::Command::new("sh")
                .arg("-c")
                .arg(&script)
                .arg(&cmd[0])
                .args(&cmd[1..])
                .current_dir(&self.root)
                .status()
                .map_err(|e| format!("bisect run: could not run '{}': {e}", cmd[0]))?;
            let verdict = match status.code() {
                Some(0) => "good",
                Some(125) => "skip",
                Some(c) if (1..128).contains(&c) => "bad",
                _ => {
                    return Err(format!(
                        "bisect run: '{}' exited with {status} on {at}; stopping",
                        cmd.join(" ")
                    )
                    .into());
                }
            };
            match self.mark(verdict, &[], out)? {
                Next::Test(..) => continue,
                Next::Found(_) => return Ok(()),
                Next::OnlySkipped(_) => {
                    return Err(
                        "bisect run: cannot bisect more, only skipped commits are left".into(),
                    );
                }
                Next::Waiting => unreachable!("run marks the commit it tested"),
            }
        }
    }

    /// Works out the next commit, checks it out and reports, as git does.
    fn bisect_step(&mut self, state: &BisectState, out: &mut impl Write) -> Res<Next> {
        let next = self.bisect_next(state)?;
        match &next {
            Next::Waiting => {
                let missing = match (state.bad, state.good.is_empty()) {
                    (None, true) => "good and bad commits",
                    (None, false) => "a bad commit, 1 good commit known",
                    _ => "good commit(s), bad commit known",
                };
                writeln!(out, "status: waiting for {missing}")?;
            }
            Next::Found(c) => {
                writeln!(out, "{c} is the first bad commit")?;
                writeln!(out, "[{c}] {}", self.subject(*c)?)?;
            }
            Next::OnlySkipped(cs) => {
                writeln!(
                    out,
                    "There are only 'skip'ped commits left to test.\n\
                     The first bad commit could be any of:"
                )?;
                for c in cs {
                    writeln!(out, "{c}")?;
                }
                writeln!(out, "We cannot bisect more!")?;
            }
            Next::Test(c, left, steps) => {
                self.ensure_clean("bisect")?;
                let at = self.bisect_head()?;
                let old = self.commit_entries(at)?;
                let target = self.commit_entries(*c)?;
                self.checkout(&old, &target)?;
                let scratch = self.bisect_ref();
                self.commit_refs(
                    "bisect",
                    &[RefChange {
                        name: scratch,
                        old: Some(RefTarget::Oid(at)),
                        new: Some(RefTarget::Oid(*c)),
                    }],
                )?;
                let s = |n: usize| if n == 1 { "" } else { "s" };
                writeln!(
                    out,
                    "Bisecting: {left} revision{} left to test after this (roughly {steps} step{})",
                    s(*left),
                    s(*steps as usize)
                )?;
                writeln!(out, "[{c}] {}", self.subject(*c)?)?;
            }
        }
        Ok(next)
    }

    /// git's bisection: among commits reachable from the bad one but from no
    /// good one, test the one whose ancestors inside that set come closest
    /// to half of it.
    fn bisect_next(&self, state: &BisectState) -> Res<Next> {
        let Some(bad) = state.bad else {
            return Ok(Next::Waiting);
        };
        if state.good.is_empty() {
            return Ok(Next::Waiting);
        }
        let good = self.ancestry(&state.good)?;
        let graph: std::collections::HashMap<ObjectId, Vec<ObjectId>> = self
            .ancestry(&[bad])?
            .into_iter()
            .filter(|(c, _)| !good.contains_key(c))
            .collect();
        let all = graph.len();
        if all == 1 {
            return Ok(Next::Found(bad));
        }
        let mut best: Option<(usize, i64, ObjectId, usize)> = None;
        for &c in graph.keys() {
            if c == bad || state.skip.contains(&c) {
                continue;
            }
            let reach = reach_within(&graph, c);
            let score = reach.min(all - reach);
            let when = self.committer_date(c)?;
            let key = (score, when, c, reach);
            // highest score; on a tie, as git, the side reaching fewer
            // candidates, then the newest, then the smallest oid
            let rank = |k: &(usize, i64, ObjectId, usize)| {
                (k.0, std::cmp::Reverse(k.3), k.1, std::cmp::Reverse(k.2))
            };
            if best.as_ref().is_none_or(|b| rank(&key) > rank(b)) {
                best = Some(key);
            }
        }
        match best {
            Some((_, _, c, reach)) => Ok(Next::Test(c, all - reach - 1, estimate_steps(all))),
            None => {
                let mut left: Vec<ObjectId> = graph.keys().copied().collect();
                left.sort();
                Ok(Next::OnlySkipped(left))
            }
        }
    }

    pub(super) fn rev(&self, r: &str) -> Res<ObjectId> {
        let repo = alt_repo::Repository::discover(&self.store.alt_dir)?;
        Ok(repo
            .rev_parse(r)?
            .ok_or_else(|| format!("bad revision '{r}'"))?)
    }

    fn subject(&self, c: ObjectId) -> Res<String> {
        let obj = self.store.odb.get(&c)?.ok_or("commit missing from store")?;
        let commit = alt_git_codec::Commit::parse(&obj.data)?;
        Ok(String::from_utf8_lossy(commit.message())
            .lines()
            .next()
            .unwrap_or("")
            .to_owned())
    }

    fn committer_date(&self, c: ObjectId) -> Res<i64> {
        let obj = self.store.odb.get(&c)?.ok_or("commit missing from store")?;
        Ok(alt_git_codec::Commit::parse(&obj.data)?
            .committer_date()
            .unwrap_or(0))
    }

    fn bisect_ref(&self) -> String {
        format!("bisect/{}", self.workspace)
    }

    fn bisect_head(&self) -> Res<ObjectId> {
        Ok(self
            .store
            .refs
            .resolve(&self.bisect_ref())?
            .ok_or("bisect: the scratch ref is missing")?)
    }

    fn bisect_state_path(&self) -> PathBuf {
        self.index_path.with_file_name("BISECT_STATE")
    }

    pub(super) fn bisect_in_progress(&self) -> bool {
        self.bisect_state_path().exists()
    }

    fn bisect_state(&self) -> Res<Option<BisectState>> {
        let text = match std::fs::read_to_string(self.bisect_state_path()) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let corrupt = || "corrupt BISECT_STATE";
        let mut state = BisectState::default();
        for line in text.lines() {
            let (key, value) = line.split_once(' ').ok_or_else(corrupt)?;
            let oid = || value.parse::<ObjectId>().map_err(|_| corrupt());
            match key {
                "branch" => state.branch = value.to_owned(),
                "bad" => state.bad = Some(oid()?),
                "good" => state.good.push(oid()?),
                "skip" => state.skip.push(oid()?),
                _ => return Err(corrupt().into()),
            }
        }
        if state.branch.is_empty() {
            return Err(corrupt().into());
        }
        Ok(Some(state))
    }

    fn save_bisect_state(&self, s: &BisectState) -> Res<()> {
        let mut text = format!("branch {}\n", s.branch);
        if let Some(b) = s.bad {
            text.push_str(&format!("bad {b}\n"));
        }
        for g in &s.good {
            text.push_str(&format!("good {g}\n"));
        }
        for k in &s.skip {
            text.push_str(&format!("skip {k}\n"));
        }
        std::fs::write(self.bisect_state_path(), text)?;
        Ok(())
    }
}

/// How many commits of `graph` are reachable from `start`, itself included.
fn reach_within(
    graph: &std::collections::HashMap<ObjectId, Vec<ObjectId>>,
    start: ObjectId,
) -> usize {
    let mut seen = std::collections::HashSet::new();
    let mut stack = vec![start];
    while let Some(c) = stack.pop() {
        if !seen.insert(c) {
            continue;
        }
        stack.extend(graph[&c].iter().filter(|p| graph.contains_key(*p)));
    }
    seen.len()
}

/// git's estimate of the steps left for `all` candidates.
fn estimate_steps(all: usize) -> u32 {
    if all < 3 {
        return 0;
    }
    let n = usize::BITS - 1 - all.leading_zeros();
    let e = 1usize << n;
    let x = all - e;
    if e < 3 * x { n } else { n - 1 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_estimates_match_git() {
        // values git's estimate_bisect_steps gives
        for (all, steps) in [
            (1, 0),
            (2, 0),
            (3, 1),
            (4, 1),
            (5, 1),
            (6, 2),
            (8, 2),
            (11, 3),
            (16, 3),
            (100, 6),
        ] {
            assert_eq!(estimate_steps(all), steps, "all={all}");
        }
    }
}
