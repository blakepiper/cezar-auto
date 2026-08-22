use super::*;

impl RunManager {
    /// Add an id to the in-memory FIFO. The run itself is already durably queued by creation or
    /// lifecycle code; queue membership is intentionally process-local.
    pub fn enqueue(&mut self, run_id: impl Into<String>) -> bool {
        self.queue.enqueue(run_id)
    }

    pub fn take_next(&mut self) -> Option<String> {
        self.queue.take_next()
    }

    pub fn finish_start(&mut self, run_id: &str) -> bool {
        self.queue.finish_start(run_id)
    }

    pub fn queue(&self) -> &QueueState {
        &self.queue
    }

    pub fn is_active(&self, run_id: &str) -> bool {
        self.active.contains_key(run_id)
            || self.jobs.contains_key(run_id)
            || self.queue.is_queued(run_id)
            || self.queue.is_starting(run_id)
    }

    pub fn recover_queued(&mut self) -> Vec<String> {
        self.queue = QueueState::default();
        for run_id in fifo_run_ids(&self.list_runs()) {
            self.queue.enqueue(run_id);
        }
        self.queue.queued().map(str::to_owned).collect()
    }

    pub fn hydrate_queued_prompt(&self, run_id: &str) -> Option<String> {
        self.get_run(run_id).map(hydrate_queued_prompt)
    }

    pub fn account_holds(&self) -> AccountHolds {
        let now = now_iso8601();
        derive_account_holds(&self.list_runs(), &now)
    }

    pub fn account_holds_at(&self, now: &str) -> AccountHolds {
        derive_account_holds(&self.list_runs(), now)
    }

    pub fn reconcile_quota_at(&self, now: &str) -> QuotaReconciliation {
        reconcile_quota(&self.list_runs(), now, Runner::Claude)
    }

    /// Apply one deterministic auto-resume pass. Future deadlines remain durable holds; only due
    /// runs whose account is not held are handed to the ordinary continuation queue.
    pub fn reconcile_auto_resumes(&mut self, now: &str) -> io::Result<AutoResumeReport> {
        let plan = self.reconcile_quota_at(now);
        let mut report = AutoResumeReport {
            plan: plan.clone(),
            ..AutoResumeReport::default()
        };
        if !self.runtime_options.auto_resume_on_usage_limit {
            return Ok(report);
        }
        for run_id in &plan.stale {
            if self.get_run(run_id).is_none() {
                continue;
            }
            self.update_run(
                run_id,
                RunPatch::new()
                    .clear("autoResumeAt")
                    .clear("autoResumeAttempts"),
            )?;
            self.append_event(
                run_id,
                EventInput::new("note")
                    .field("message", "automatic resume retired during reconciliation"),
            )?;
            report.retired.push(run_id.clone());
        }
        for run_id in &plan.due {
            let attempts = self
                .get_run(run_id)
                .and_then(|run| run.auto_resume_attempts)
                .unwrap_or(0.0)
                + 1.0;
            let result = self.continue_run(
                run_id,
                ContinueOptions {
                    text: Some(
                        "The provider usage limit has reset. Continue the task from its last durable state."
                            .to_owned(),
                    ),
                    ..ContinueOptions::default()
                },
            )?;
            if result.ok {
                let status = self.get_run(run_id).map(|run| run.status);
                if matches!(
                    status,
                    Some(
                        RunStatus::Queued
                            | RunStatus::Running
                            | RunStatus::Idle
                            | RunStatus::Waiting,
                    )
                ) {
                    self.update_run(run_id, RunPatch::new().set("autoResumeAttempts", attempts))?;
                    self.append_event(
                        run_id,
                        EventInput::new("lifecycle").field(
                            "message",
                            format!("usage limit reset — resuming automatically ({attempts:.0}/{MAX_AUTO_RESUME_ATTEMPTS:.0})"),
                        ),
                    )?;
                    report.requeued.push(run_id.clone());
                } else if matches!(status, Some(RunStatus::Done | RunStatus::Review)) {
                    report.requeued.push(run_id.clone());
                } else {
                    report.retired.push(run_id.clone());
                }
            } else {
                self.update_run(
                    run_id,
                    RunPatch::new()
                        .clear("autoResumeAt")
                        .clear("autoResumeAttempts"),
                )?;
                self.append_event(
                    run_id,
                    EventInput::new("note").field(
                        "message",
                        format!(
                            "automatic resume could not start — {}",
                            result.error.unwrap_or_else(|| "unknown".to_owned())
                        ),
                    ),
                )?;
                report.retired.push(run_id.clone());
            }
        }
        self.pump()?;
        Ok(report)
    }

    /// Re-adopt durable live records after a process restart. No session object is assumed to
    /// survive: queued work is rebuilt from `workflow_def`, waiting work is settled, and running
    /// work is marked interrupted before it is requeued through the normal continuation path.
    pub fn recover(&mut self) -> io::Result<RecoveryReport> {
        let mut live = self
            .list_runs()
            .into_iter()
            .filter(|run| {
                matches!(
                    run.status,
                    RunStatus::Queued | RunStatus::Idle | RunStatus::Waiting | RunStatus::Running
                )
            })
            .collect::<Vec<_>>();
        live.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        let mut report = RecoveryReport::default();
        for run in live {
            if self.is_active(&run.id) {
                continue;
            }
            match run.status {
                RunStatus::Queued => {
                    let Some(workflow) = run.workflow_def.clone() else {
                        self.fail_run(
                            &run.id,
                            None,
                            "interrupted — workflow definition not recoverable after a restart"
                                .to_owned(),
                        )?;
                        report.failed.push(run.id);
                        continue;
                    };
                    let start_index = workflow
                        .steps
                        .iter()
                        .position(|step| {
                            self.get_run(&run.id)
                                .and_then(|record| {
                                    record
                                        .steps
                                        .iter()
                                        .find(|candidate| candidate.id == step.id)
                                })
                                .is_none_or(|record| record.status != StepStatus::Done)
                        })
                        .unwrap_or(workflow.steps.len());
                    self.jobs.insert(
                        run.id.clone(),
                        RuntimeJob::Workflow {
                            workflow,
                            start_index,
                            retry_counts: BTreeMap::new(),
                        },
                    );
                    self.enqueue(run.id.clone());
                    report.queued.push(run.id);
                }
                RunStatus::Idle | RunStatus::Waiting => {
                    let finished_at = now_iso8601();
                    for step in &run.steps {
                        if matches!(step.status, StepStatus::Waiting | StepStatus::Running) {
                            self.update_step(
                                &run.id,
                                &step.id,
                                StepPatch::new()
                                    .set("status", StepStatus::Done)
                                    .set("finishedAt", finished_at.clone()),
                            )?;
                        }
                    }
                    self.append_event(
                        &run.id,
                        EventInput::new("lifecycle")
                            .field("message", "process restarted — waiting session settled"),
                    )?;
                    self.settle_success(&run.id)?;
                    report.settled.push(run.id);
                }
                RunStatus::Running => {
                    let session_exists = run.steps.iter().any(|step| step.session_id.is_some());
                    let finished_at = now_iso8601();
                    for step in &run.steps {
                        if matches!(step.status, StepStatus::Running | StepStatus::Waiting) {
                            self.update_step(
                                &run.id,
                                &step.id,
                                StepPatch::new()
                                    .set("status", StepStatus::Failed)
                                    .set("finishedAt", finished_at.clone()),
                            )?;
                        }
                    }
                    self.update_run(
                        &run.id,
                        RunPatch::new()
                            .set("status", RunStatus::Failed)
                            .set("error", "interrupted — process exited during the run")
                            .set("finishedAt", finished_at)
                            .clear("currentStepId"),
                    )?;
                    if session_exists {
                        let result = self.continue_run(
                            &run.id,
                            ContinueOptions {
                                text: Some(
                                    "The process restarted while this task was running. Continue from the last durable state."
                                        .to_owned(),
                                ),
                                ..ContinueOptions::default()
                            },
                        )?;
                        self.append_event(
                            &run.id,
                            EventInput::new("lifecycle").field(
                                "message",
                                if result.ok {
                                    "process restarted — interrupted task requeued"
                                } else {
                                    "process restarted — interrupted task could not resume"
                                },
                            ),
                        )?;
                        if result.ok {
                            report.resumed.push(run.id);
                        } else {
                            report.failed.push(run.id);
                        }
                    } else {
                        self.append_event(
                            &run.id,
                            EventInput::new("lifecycle").field(
                                "message",
                                "process restarted — no session was available for continuation",
                            ),
                        )?;
                        report.failed.push(run.id);
                    }
                }
                RunStatus::Done | RunStatus::Failed | RunStatus::Cancelled | RunStatus::Review => {
                    continue;
                }
            }
        }
        let _ = self.reconcile_auto_resumes(&now_iso8601())?;
        self.run_to_completion()?;
        Ok(report)
    }

    /// Drain queued jobs while an injected runtime slot is available. The method is synchronous on
    /// purpose: the engine can call it from its scheduler, while unit tests can observe every
    /// transition without sleeps or a process-wide executor.
    /// Admit as much of the queue as current capacity allows. This never opens a session or runs
    /// a provider turn — `execute_job` stops the moment a job needs a live session and records an
    /// [`AdmittedTurn`] instead, so this always returns quickly regardless of how long a provider
    /// turn takes. A caller that wants those turns to actually run must drain
    /// [`Self::take_pending_turns`] afterward and dispatch each one to its own worker, outside any
    /// lock on this manager.
    pub fn pump(&mut self) -> io::Result<()> {
        loop {
            if !self.capacity_available() {
                break;
            }
            let Some(run_id) = self.queue.take_next() else {
                break;
            };
            let Some(job) = self.jobs.remove(&run_id) else {
                self.queue.finish_start(&run_id);
                continue;
            };
            if !self.try_acquire_resources(&run_id) {
                self.jobs.insert(run_id.clone(), job);
                self.queue.finish_start(&run_id);
                self.queue.push_front(run_id);
                break;
            }
            let result = self.execute_job(&run_id, job);
            self.queue.finish_start(&run_id);
            result?;
        }
        Ok(())
    }

    /// Drain every turn `pump` admitted but did not run. A caller must call this after any
    /// operation that could have called `pump` (directly or via a durable-state transition that
    /// re-admits queued work) and dispatch each returned turn to its own worker.
    pub fn take_pending_turns(&mut self) -> Vec<AdmittedTurn> {
        self.pending_turns.drain(..).collect()
    }

    /// Single-threaded stand-in for the production per-run worker coordinator: opens and runs
    /// every admitted turn synchronously (against whatever `session_factory` was configured),
    /// applying results the same way a concurrent worker would, until nothing is admittable. This
    /// is what a genuinely single-shot, no-TUI caller (the headless `coducktor run` CLI, and this
    /// module's own tests) wants — one turn at a time, blocking the caller until the whole
    /// workflow settles, with no separate coordinator thread to stand up.
    pub fn run_to_completion(&mut self) -> io::Result<()> {
        loop {
            self.pump()?;
            let admitted = self.take_pending_turns();
            if admitted.is_empty() {
                return Ok(());
            }
            for turn in admitted {
                self.drive_admitted_turn_sync(turn)?;
            }
        }
    }

    pub(super) fn drive_admitted_turn_sync(&mut self, admitted: AdmittedTurn) -> io::Result<()> {
        let run_id = admitted.run_id.clone();
        let mut step_id = admitted.step_id.clone();
        let mut factory = self.session_factory.take();
        let opened = match factory.as_mut() {
            Some(factory) => factory.open(admitted.request.clone()),
            None => Err("session factory unavailable".to_owned()),
        };
        self.session_factory = factory;
        let cancellation_requested = admitted.request.cancellation.is_requested();
        let mut session = match opened {
            Ok(session) => session,
            Err(error) => {
                return self.apply_open_failure(admitted, error, cancellation_requested);
            }
        };
        let turn_result =
            session.turn(&mut |event| self.apply_turn_event(&run_id, &step_id, event));
        let mut step =
            self.apply_admitted_turn(admitted, session, turn_result, cancellation_requested)?;
        loop {
            let (mut active, prompt, images, queued) = match step {
                TurnStep::Nudge(active) => (active, AUTONOMOUS_NUDGE.to_owned(), Vec::new(), false),
                TurnStep::QueuedMessage {
                    active,
                    prompt,
                    images,
                } => (active, prompt, images, true),
                TurnStep::GitAutoCommit(_) => {
                    self.finish_git_auto(
                        &run_id,
                        Err(
                            "automatic Git actions require the production engine dispatcher"
                                .to_owned(),
                        ),
                    )?;
                    break;
                }
                TurnStep::Done => break,
            };
            step_id = self.checked_active_step_id(&run_id, &active)?;
            let send_result = active
                .session_mut()
                .send_message(&prompt, &images, &mut |event| {
                    self.apply_turn_event(&run_id, &step_id, event)
                });
            step = if queued {
                self.apply_message_turn(&run_id, *active, send_result, false)?
            } else {
                self.apply_active_turn(&run_id, *active, send_result, false)?
            };
        }
        Ok(())
    }

    pub(super) fn runtime_busy_slots(&self) -> usize {
        self.active
            .values()
            .filter(|active| active.holds_slot)
            .count()
            .saturating_add(self.queue.starting().count())
            .saturating_add(self.in_flight.len())
    }

    pub(super) fn capacity_available(&self) -> bool {
        if self.runtime_busy_slots() >= self.runtime_options.max_parallel {
            return false;
        }
        self.workspace_semaphore
            .as_ref()
            .is_none_or(|semaphore| semaphore.busy_slots() < semaphore.max_parallel())
    }

    pub(super) fn try_acquire_resources(&mut self, run_id: &str) -> bool {
        let project_id = self.project_id.clone();
        if !self.workspace_holds.contains(run_id) {
            let acquired = self
                .workspace_semaphore
                .as_mut()
                .is_none_or(|semaphore| semaphore.try_acquire(run_id, &project_id));
            if !acquired {
                return false;
            }
            if self.workspace_semaphore.is_some() {
                self.workspace_holds.insert(run_id.to_owned());
            }
        }
        // A worktree has an independent checkout, so it must not be serialized with another
        // worktree (or an in-place run) merely because both came from the same repository. The
        // integration layer still installs the root lease for in-place runs, whose checkout is
        // shared and therefore unsafe to mutate concurrently.
        let needs_repository_lease = self
            .get_run(run_id)
            .is_none_or(|run| run.worktree_path.is_none());
        if needs_repository_lease && !self.repository_holds.contains(run_id) {
            let acquired = self
                .repository_lease
                .as_mut()
                .is_none_or(|lease| lease.try_acquire(run_id));
            if !acquired {
                self.release_workspace_hold(run_id);
                return false;
            }
            if self.repository_lease.is_some() {
                self.repository_holds.insert(run_id.to_owned());
            }
        }
        true
    }

    pub(super) fn release_workspace_hold(&mut self, run_id: &str) {
        if self.workspace_holds.remove(run_id)
            && let Some(semaphore) = self.workspace_semaphore.as_mut()
        {
            semaphore.release(run_id, &self.project_id);
        }
    }

    pub(super) fn try_acquire_workspace_resume(&mut self, run_id: &str) -> bool {
        if self.workspace_holds.contains(run_id) {
            return true;
        }
        let acquired = self
            .workspace_semaphore
            .as_mut()
            .is_none_or(|semaphore| semaphore.try_acquire(run_id, &self.project_id));
        if acquired && self.workspace_semaphore.is_some() {
            self.workspace_holds.insert(run_id.to_owned());
        }
        acquired
    }

    pub(super) fn release_repository_hold(&mut self, run_id: &str) {
        if self.repository_holds.remove(run_id)
            && let Some(lease) = self.repository_lease.as_mut()
        {
            lease.release(run_id);
        }
    }

    pub(super) fn execute_job(&mut self, run_id: &str, job: RuntimeJob) -> io::Result<()> {
        let (workflow, mut index, mut retry_counts, continuation) = match job {
            RuntimeJob::Workflow {
                workflow,
                start_index,
                retry_counts,
            } => (workflow, start_index, retry_counts, None),
            RuntimeJob::Continuation {
                workflow,
                step_index,
                session_id,
                prompt,
                images,
                runner,
                model,
                retry_counts,
            } => (
                workflow,
                step_index,
                retry_counts,
                Some((session_id, prompt, images, runner, model)),
            ),
        };
        let Some(record) = self.get_run(run_id).cloned() else {
            return Ok(());
        };
        let plan_checkpoint = self.plan_checkpoints.remove(run_id).unwrap_or_default();
        let mut pending_context_prompt = self.pending_context_prompts.remove(run_id);

        if continuation.is_some() {
            self.update_run(
                run_id,
                RunPatch::new()
                    .set("status", RunStatus::Running)
                    .clear("error")
                    .clear("finishedAt")
                    .clear("currentStepId")
                    .set("activity", Value::Null),
            )?;
        } else {
            let started_at = record.started_at.unwrap_or_else(now_iso8601);
            self.update_run(
                run_id,
                RunPatch::new()
                    .set("status", RunStatus::Running)
                    .set("startedAt", started_at)
                    .clear("error")
                    .clear("finishedAt"),
            )?;
            while index < workflow.steps.len()
                && self
                    .get_run(run_id)
                    .and_then(|run| run.steps.get(index))
                    .is_some_and(|step| step.status == StepStatus::Done)
            {
                index += 1;
            }
        }

        let mut continuation_prompt = continuation
            .as_ref()
            .map(|(_, prompt, _, _, _)| prompt.clone());
        let mut continuation_images = continuation
            .as_ref()
            .map(|(_, _, images, _, _)| images.clone())
            .unwrap_or_default();
        let continuation_session = continuation
            .as_ref()
            .and_then(|(session_id, _, _, _, _)| session_id.clone());
        let continuation_runner = continuation.as_ref().map(|(_, _, _, runner, _)| *runner);
        let continuation_model = continuation
            .as_ref()
            .and_then(|(_, _, _, _, model)| model.clone());
        let continuation_step = continuation.is_some();

        while index < workflow.steps.len() {
            let step = workflow_step(&workflow, index)?.clone();
            self.update_run(
                run_id,
                RunPatch::new().set("currentStepId", step.id.clone()),
            )?;
            let iteration = self
                .get_run(run_id)
                .and_then(|run| run.steps.iter().find(|candidate| candidate.id == step.id))
                .map(|step| step.iterations + 1.0)
                .unwrap_or(1.0);
            self.update_step(
                run_id,
                &step.id,
                StepPatch::new()
                    .set("status", StepStatus::Running)
                    .set("iterations", iteration)
                    .set("startedAt", now_iso8601())
                    .clear("finishedAt")
                    .clear("error"),
            )?;
            self.append_step_event(run_id, &step, "step-start", iteration)?;

            if let Some(command) = step.command.as_deref() {
                let cwd = self
                    .get_run(run_id)
                    .and_then(|run| run.worktree_path.as_deref())
                    .map(PathBuf::from)
                    .unwrap_or_else(|| self.repo_root());
                let result = match self.check_executor.as_mut() {
                    Some(executor) => executor.run(command, &cwd),
                    None => Err("check executor unavailable".to_owned()),
                };
                let result = match result {
                    Ok(result) => result,
                    Err(error) => CheckResult {
                        success: false,
                        exit_code: 1,
                        output: error,
                    },
                };
                self.append_event(
                    run_id,
                    EventInput::new("check-output")
                        .step(step.id.clone())
                        .field("command", command)
                        .field("text", result.output.clone())
                        .field("exitCode", result.exit_code),
                )?;
                if result.success {
                    self.complete_step(run_id, &step.id, None)?;
                    index += 1;
                    continue;
                }

                let used = retry_counts.get(&step.id).copied().unwrap_or(0);
                let retry_target = step
                    .on_fail
                    .as_ref()
                    .filter(|policy| lifecycle::retry_allowed(used, policy.max))
                    .and_then(|policy| {
                        workflow
                            .steps
                            .iter()
                            .position(|candidate| candidate.id == policy.retry)
                            .map(|target| (target, policy.retry.clone(), used + 1, policy.max))
                    });
                if let Some((target, target_id, attempt, max)) = retry_target {
                    retry_counts.insert(step.id.clone(), attempt);
                    self.update_step(
                        run_id,
                        &step.id,
                        StepPatch::new()
                            .set("status", StepStatus::Failed)
                            .set("error", "check failed — looping back")
                            .set("finishedAt", now_iso8601()),
                    )?;
                    for retry_index in target..=index {
                        if let Some(retry_step) = workflow.steps.get(retry_index) {
                            self.update_step(
                                run_id,
                                &retry_step.id,
                                StepPatch::new()
                                    .set("status", StepStatus::Pending)
                                    .clear("error")
                                    .clear("finishedAt"),
                            )?;
                        }
                    }
                    self.append_event(
                        run_id,
                        EventInput::new("note")
                            .step(step.id.clone())
                            .field(
                                "message",
                                format!(
                                    "check failed — retrying from \"{target_id}\" (attempt {attempt}/{max})"
                                ),
                            ),
                    )?;
                    index = target;
                    continue;
                }

                let attempts = used + 1;
                self.fail_run(
                    run_id,
                    Some(&step.id),
                    format!(
                        "check \"{}\" failed{}",
                        step.id,
                        step.on_fail
                            .as_ref()
                            .map(|_| format!(" after {attempts} attempts"))
                            .unwrap_or_default()
                    ),
                )?;
                return Ok(());
            }

            let task = self
                .get_run(run_id)
                .map(hydrate_queued_prompt)
                .unwrap_or_default();
            let prompt = pending_context_prompt
                .take()
                .or_else(|| continuation_prompt.take())
                .unwrap_or_else(|| {
                    apply_template(step.prompt.as_deref().unwrap_or("{{task}}"), &task)
                });
            let prompt = if !continuation_step {
                if let Some(note) = types::chain_step_note(&workflow.steps, index) {
                    format!("{note}\n\n---\n\n{prompt}")
                } else {
                    prompt
                }
            } else {
                prompt
            };
            let requested_runner = continuation_runner
                .or(step.runner)
                .or_else(|| self.get_run(run_id).and_then(|run| run.requested_runner))
                .unwrap_or(RunnerSelection::Claude);
            let runner = if requested_runner == RunnerSelection::Auto {
                self.get_run(run_id)
                    .and_then(|run| run.runner)
                    .map(runner_selection)
                    .unwrap_or(RunnerSelection::Claude)
            } else {
                requested_runner
            };
            let model = continuation_model
                .clone()
                .or_else(|| self.get_run(run_id).and_then(|run| run.model.clone()));
            let session_id = if continuation_step
                && index
                    == workflow
                        .steps
                        .iter()
                        .position(|candidate| candidate.id == step.id)
                        .unwrap_or(index)
            {
                continuation_session.clone()
            } else {
                None
            };
            let allowed_tools = step.allowed_tools.clone().unwrap_or_else(|| {
                types::DEFAULT_ALLOWED_TOOLS
                    .iter()
                    .map(|tool| (*tool).to_owned())
                    .collect()
            });
            let bash_allowlist = step.bash_allowlist.clone().unwrap_or_default();
            let run_record = self.get_run(run_id);
            let system_prompt = Some(system_prompt_with_task_controls(
                run_record.and_then(|run| run.system_prompt.as_deref()),
            ));
            let reasoning_effort = run_record
                .and_then(|run| run.reasoning_effort)
                .and_then(concrete_reasoning_effort);
            let cancellation = CancellationToken::default();
            let retry_prompt = prompt.clone();
            let request = SessionRequest {
                run_id: run_id.to_owned(),
                step_id: step.id.clone(),
                prompt,
                images: if continuation_step {
                    std::mem::take(&mut continuation_images)
                } else {
                    self.get_run(run_id)
                        .map(hydrate_queued_images)
                        .unwrap_or_default()
                },
                runner,
                model,
                session_id,
                continuation: continuation_step,
                agent_profile: self
                    .get_run(run_id)
                    .and_then(|run| run.agent_profile.clone()),
                env: BTreeMap::new(),
                cwd: self
                    .get_run(run_id)
                    .and_then(|run| run.worktree_path.as_deref())
                    .map(PathBuf::from)
                    .unwrap_or_else(|| self.repo_root()),
                allowed_tools,
                bash_allowlist,
                system_prompt,
                reasoning_effort,
                cancellation: cancellation.clone(),
            };
            self.acknowledge_queued_messages(run_id, &step.id)?;
            let mut run_affinity = RunPatch::new();
            if let Some(backend) = concrete_runner(runner) {
                run_affinity = run_affinity.set("runner", backend);
            }
            if self
                .get_run(run_id)
                .and_then(|run| run.requested_runner)
                .is_none()
            {
                run_affinity = run_affinity.set("requestedRunner", runner);
            }
            if !run_affinity.fields().is_empty() {
                self.update_run(run_id, run_affinity)?;
            }
            let mut step_affinity = StepPatch::new().set("requestedRunner", requested_runner);
            if let Some(backend) = concrete_runner(runner) {
                step_affinity = step_affinity.set("backend", backend);
            }
            if let Some(profile_id) = self
                .get_run(run_id)
                .and_then(|run| run.agent_profile.clone())
            {
                step_affinity = step_affinity.set("profileId", profile_id);
            }
            self.update_step(run_id, &step.id, step_affinity)?;
            let concrete = concrete_runner(runner).unwrap_or(Runner::Claude);
            if requested_runner == RunnerSelection::Auto {
                self.announce_auto_route(run_id, concrete, request.model.as_deref())?;
            }
            // Opening and running this turn is deliberately not done here: both can block for the
            // lifetime of a provider turn, and this function runs under the manager's lock. The
            // caller (a per-run worker, outside any lock) opens `request` and runs the turn, then
            // reports back through `apply_open_failure`/`apply_admitted_turn` so this workflow can
            // resume exactly where it left off.
            self.in_flight.insert(run_id.to_owned());
            self.pending_turns.push_back(AdmittedTurn {
                run_id: run_id.to_owned(),
                step_id: step.id.clone(),
                request,
                resume: PendingResume {
                    workflow,
                    index,
                    retry_counts,
                    plan_checkpoint,
                    concrete,
                    retry_prompt,
                },
            });
            return Ok(());
        }

        self.settle_success(run_id)
    }

    /// Requeue a run for the given step so a fresh `execute_job` call rebuilds and reattempts its
    /// turn — used by both open-failure and turn-failure auto-failover retries, which durably
    /// mutate the run's runner before asking for another attempt.
    pub(super) fn requeue_for_retry(
        &mut self,
        run_id: &str,
        workflow: WorkflowDef,
        index: usize,
        retry_counts: BTreeMap<String, u32>,
        retry_prompt: String,
    ) -> io::Result<()> {
        self.pending_context_prompts
            .insert(run_id.to_owned(), retry_prompt);
        self.jobs.insert(
            run_id.to_owned(),
            RuntimeJob::Workflow {
                workflow,
                start_index: index,
                retry_counts,
            },
        );
        self.enqueue(run_id.to_owned());
        self.pump()
    }

    /// Apply the result of attempting to open a session for an [`AdmittedTurn`]. Mirrors the
    /// pre-refactor open-failure branch: cancellation wins outright, then auto-failover retries by
    /// requeuing (a fresh `execute_job` call rebuilds the request against the newly selected
    /// runner), otherwise the run fails.
    pub fn apply_open_failure(
        &mut self,
        admitted: AdmittedTurn,
        error: String,
        cancellation_requested: bool,
    ) -> io::Result<()> {
        let AdmittedTurn {
            run_id,
            step_id,
            resume,
            ..
        } = admitted;
        self.in_flight.remove(&run_id);
        if cancellation_requested {
            return self.cancel_run_after_session(&run_id, &step_id);
        }
        if self.try_auto_failover(&run_id, &step_id, resume.concrete, &error, true)? {
            return self.requeue_for_retry(
                &run_id,
                resume.workflow,
                resume.index,
                resume.retry_counts,
                resume.retry_prompt,
            );
        }
        self.fail_run(&run_id, Some(&step_id), error)
    }

    /// Fail a run that a caller admitted but could not even hand to a worker — e.g. the OS
    /// refused to start a thread. A local resource failure, not a provider one, so unlike
    /// [`Self::apply_open_failure`] this never retries through auto-failover; it just needs the
    /// run's id, not the full [`AdmittedTurn`] (deliberately, so a caller that already lost the
    /// value moving it toward a worker that never started can still report the failure).
    pub fn fail_admission(
        &mut self,
        run_id: &str,
        step_id: &str,
        reason: String,
    ) -> io::Result<()> {
        self.in_flight.remove(run_id);
        self.fail_run(run_id, Some(step_id), reason)
    }

    /// Apply the result of a successfully opened session's first turn. `turn_result` is exactly
    /// what `AgentSession::turn` returned, run entirely outside the manager's lock by the caller.
    pub fn apply_admitted_turn(
        &mut self,
        admitted: AdmittedTurn,
        session: Box<dyn AgentSession + Send>,
        turn_result: Result<SessionOutcome, String>,
        cancellation_requested: bool,
    ) -> io::Result<TurnStep> {
        let AdmittedTurn {
            run_id,
            step_id: _,
            resume,
            ..
        } = admitted;
        let outcome = match turn_result {
            Ok(_) if cancellation_requested => SessionOutcome::Cancelled(SessionReport::default()),
            Ok(outcome) => outcome,
            Err(_) if cancellation_requested => SessionOutcome::Cancelled(SessionReport::default()),
            Err(error) => {
                self.in_flight.remove(&run_id);
                let step_id = match workflow_step(&resume.workflow, resume.index) {
                    Ok(step) => step.id.clone(),
                    Err(error) => {
                        self.in_flight.remove(&run_id);
                        self.fail_run(&run_id, None, error.to_string())?;
                        return Ok(TurnStep::Done);
                    }
                };
                if self.try_auto_failover(&run_id, &step_id, resume.concrete, &error, false)? {
                    self.requeue_for_retry(
                        &run_id,
                        resume.workflow,
                        resume.index,
                        resume.retry_counts,
                        resume.retry_prompt,
                    )?;
                    return Ok(TurnStep::Done);
                }
                self.fail_run(&run_id, Some(&step_id), error)?;
                return Ok(TurnStep::Done);
            }
        };
        let active = RuntimeActive {
            workflow: resume.workflow,
            step_index: resume.index,
            next_index: resume.index + 1,
            retry_counts: resume.retry_counts,
            session,
            holds_slot: true,
            plan_checkpoint: resume.plan_checkpoint,
            auto_continues: 0,
            failover: Some(FailoverContext {
                concrete: resume.concrete,
                retry_prompt: resume.retry_prompt,
            }),
        };
        self.continue_active_turn(&run_id, active, outcome)
    }

    /// Apply the result of an autonomous nudge (`AgentSession::send_message`) the caller sent
    /// after a prior [`TurnStep::Nudge`]. Behaves exactly like the initial turn's wrapping: a
    /// cancellation request wins over whatever the session actually returned.
    pub fn apply_active_turn(
        &mut self,
        run_id: &str,
        active: RuntimeActive,
        send_result: Result<SessionOutcome, String>,
        cancellation_requested: bool,
    ) -> io::Result<TurnStep> {
        let outcome = match send_result {
            Ok(_) if cancellation_requested => SessionOutcome::Cancelled(SessionReport::default()),
            Ok(outcome) => outcome,
            Err(_) if cancellation_requested => SessionOutcome::Cancelled(SessionReport::default()),
            Err(error) => SessionOutcome::Failed {
                message: error,
                report: SessionReport::default(),
            },
        };
        self.continue_active_turn(run_id, active, outcome)
    }

    /// Every currently live-in-process monitoring session whose durable `monitoringWakeAt`
    /// deadline has passed. Read-only and cheap — a caller (a dedicated scheduler, not this
    /// manager's own admission loop) polls this on a bounded interval and dispatches each one
    /// through [`Self::begin_monitoring_wake`] the same way it would an [`AdmittedTurn`].
    pub fn due_monitoring_wakes(&self, now: &str) -> Vec<String> {
        self.runs
            .values()
            .filter(|run| self.active.contains_key(&run.id))
            .filter(|run| monitoring::is_due(run, now))
            .map(|run| run.id.clone())
            .collect()
    }

    /// Detach a due monitoring session from `self.active` so its check-in turn can run outside
    /// this manager's lock, exactly like a freshly admitted turn. Re-validates against the
    /// current durable record rather than trusting an earlier `due_monitoring_wakes` call, since
    /// state can change between planning and dispatch (a real user message, a cancel). Counts as
    /// `in_flight` for the same reason an admitted turn does: `RunManager::cancel` must not race
    /// the caller's eventual `apply_active_turn` report by settling the run out from under it.
    pub fn begin_monitoring_wake(&mut self, run_id: &str) -> io::Result<Option<RuntimeActive>> {
        if self.get_run(run_id).and_then(|run| run.activity) != Some(RunActivity::Monitoring) {
            return Ok(None);
        }
        if !self.active.contains_key(run_id) {
            return Ok(None);
        }
        self.append_event(
            run_id,
            EventInput::new("note").field("message", "monitoring check-in"),
        )?;
        self.in_flight.insert(run_id.to_owned());
        Ok(self.active.remove(run_id))
    }

    /// A monitoring wake's worker could not even be started (e.g. the OS refused a new thread).
    /// Put the still-live session back exactly as `park_session` would have left it, rather than
    /// leaking it as permanently `in_flight` with no session anywhere to resolve it — the next
    /// scheduler pass, or an explicit cancel/message, can still reach it normally.
    pub fn abandon_monitoring_wake(&mut self, run_id: &str, active: RuntimeActive) {
        self.in_flight.remove(run_id);
        self.active.insert(run_id.to_owned(), active);
    }

    pub(super) fn apply_session_report(
        &mut self,
        run_id: &str,
        step_id: &str,
        report: &SessionReport,
        fallback_session_id: Option<String>,
    ) -> io::Result<()> {
        let Some(step) = self
            .get_run(run_id)
            .and_then(|run| run.steps.iter().find(|step| step.id == step_id))
            .cloned()
        else {
            return Ok(());
        };
        let mut patch = StepPatch::new();
        if let Some(session_id) = report.session_id.clone().or(fallback_session_id) {
            patch = patch.set("sessionId", session_id);
        }
        if report.tokens_used.is_finite() && report.tokens_used != 0.0 {
            patch = patch.set("tokensUsed", step.tokens_used + report.tokens_used);
        }
        if let Some(input) = report
            .input_tokens
            .filter(|value| value.is_finite() && *value >= 0.0)
        {
            patch = patch.set("inputTokens", step.input_tokens.unwrap_or(0.0) + input);
        }
        if let Some(output) = report
            .output_tokens
            .filter(|value| value.is_finite() && *value >= 0.0)
        {
            patch = patch.set("outputTokens", step.output_tokens.unwrap_or(0.0) + output);
        }
        if let Some(cost) = report.cost_usd.filter(|value| value.is_finite()) {
            patch = patch.set("costUsd", step.cost_usd.unwrap_or(0.0) + cost);
        }
        if !patch.fields().is_empty() {
            self.update_step(run_id, step_id, patch)?;
        }
        Ok(())
    }

    /// Post-turn marker bookkeeping (`DUCK:DONE`/`DUCK:PR=`/…) over the whole aggregated turn text.
    /// This no longer persists the text itself as an event — the live [`Self::event_sink`]
    /// already streamed it turn-by-turn as the session produced it; re-appending the aggregate
    /// here would duplicate the transcript.
    pub(super) fn apply_session_markers(&mut self, run_id: &str, text: &str) -> io::Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        self.apply_turn_markers(run_id, text).map(|_| ())
    }

    pub(super) fn append_step_event(
        &mut self,
        run_id: &str,
        step: &coducktor_contract::workflows::WorkflowStepDef,
        event_type: &str,
        iteration: f64,
    ) -> io::Result<()> {
        self.append_event(
            run_id,
            EventInput::new(event_type)
                .step(step.id.clone())
                .field("name", step.name.clone().unwrap_or_else(|| step.id.clone()))
                .field(
                    "kind",
                    if step.command.is_some() {
                        StepKind::Check
                    } else {
                        StepKind::Agent
                    },
                )
                .field("iteration", iteration),
        )?;
        Ok(())
    }

    pub(super) fn complete_step(
        &mut self,
        run_id: &str,
        step_id: &str,
        error: Option<&str>,
    ) -> io::Result<()> {
        self.update_step(
            run_id,
            step_id,
            StepPatch::new()
                .set("status", StepStatus::Done)
                .set("finishedAt", now_iso8601())
                .set("error", error),
        )?;
        self.append_event(
            run_id,
            EventInput::new("step-end")
                .step(step_id.to_owned())
                .field("status", StepStatus::Done),
        )?;
        Ok(())
    }
}
