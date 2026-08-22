use super::*;

impl RunManager {
    pub(super) fn park_session(
        &mut self,
        run_id: &str,
        mut active: RuntimeActive,
        decision: Option<TurnMarkerDecision>,
    ) -> io::Result<()> {
        // Failover eligibility belongs to the live turn that hit the failure, never to a later,
        // separately triggered resume (`deliver_message`/`finish`) of this same parked session.
        active.failover = None;
        let monitoring = decision == Some(TurnMarkerDecision::Monitoring)
            && self
                .runs
                .iter()
                .filter(|(id, run)| {
                    id.as_str() != run_id && run.activity == Some(RunActivity::Monitoring)
                })
                .count()
                < self.runtime_options.max_monitoring_sessions;
        if active.holds_slot {
            let _ = self.try_acquire_workspace_resume(run_id);
        } else {
            self.release_workspace_hold(run_id);
        }
        let running = monitoring || active.holds_slot;
        let status = if running {
            RunStatus::Running
        } else if matches!(
            decision,
            Some(TurnMarkerDecision::Ask | TurnMarkerDecision::Waiting)
        ) {
            RunStatus::Waiting
        } else {
            RunStatus::Idle
        };
        let activity = monitoring.then_some(RunActivity::Monitoring);
        let monitoring_wake_at = monitoring.then(|| {
            self.runtime_options
                .monitoring_wake_interval_minutes
                .map(|minutes| now_plus_iso8601(Duration::from_secs(minutes.saturating_mul(60))))
        });
        self.update_run(
            run_id,
            RunPatch::new()
                .set("status", status)
                .set("activity", activity)
                .set("monitoringWakeAt", monitoring_wake_at.flatten()),
        )?;
        let step_id = self.checked_active_step_id(run_id, &active)?;
        self.update_step(
            run_id,
            &step_id,
            StepPatch::new().set(
                "status",
                if running {
                    StepStatus::Running
                } else {
                    StepStatus::Waiting
                },
            ),
        )?;
        self.active.insert(run_id.to_owned(), active);
        self.append_event(
            run_id,
            EventInput::new("lifecycle").field(
                "message",
                if monitoring {
                    "session parked for monitoring"
                } else if running {
                    "session remains active"
                } else if status == RunStatus::Waiting {
                    "session waiting for input"
                } else {
                    "session ready for follow-up"
                },
            ),
        )?;
        Ok(())
    }

    pub(super) fn begin_queued_message_turn(
        &mut self,
        run_id: &str,
        mut active: RuntimeActive,
        message: QueuedMessage,
    ) -> io::Result<TurnStep> {
        active.failover = None;
        active.auto_continues = 0;
        let step_id = self.checked_active_step_id(run_id, &active)?;
        let images = message
            .images
            .iter()
            .flatten()
            .filter_map(|url| PromptImage::from_data_url(url))
            .collect::<Vec<_>>();
        self.append_user_message(run_id, &step_id, &message.text, &images)?;
        self.edit_run(run_id, |record| {
            let mut empty = false;
            if let Some(messages) = record.queued_messages.as_mut() {
                messages.retain(|candidate| candidate.id != message.id);
                empty = messages.is_empty();
            }
            if empty {
                record.queued_messages = None;
            }
        })?;
        self.update_run(
            run_id,
            RunPatch::new()
                .set("status", RunStatus::Running)
                .clear("activity"),
        )?;
        self.update_step(
            run_id,
            &step_id,
            StepPatch::new().set("status", StepStatus::Running),
        )?;
        Ok(TurnStep::QueuedMessage {
            active: Box::new(active),
            prompt: message.text,
            images,
        })
    }

    /// Apply one turn's outcome to a live, in-progress session and decide what happens next.
    /// Shared by both a freshly admitted turn's worker (`apply_admitted_turn`/`apply_active_turn`,
    /// where `active.failover` carries the original open attempt's failover eligibility) and the
    /// synchronous `deliver_message`/`finish` resume of an already-parked session (where it is
    /// always `None` — auto-failover never applies to a resumed session, matching prior
    /// behavior). A `Nudge` result means the caller must run one more send_message turn itself
    /// (outside any lock for a worker; inline, still under the lock, for a synchronous resume)
    /// and call this again with the result.
    pub(super) fn continue_active_turn(
        &mut self,
        run_id: &str,
        mut active: RuntimeActive,
        outcome: SessionOutcome,
    ) -> io::Result<TurnStep> {
        let step_id = self.checked_active_step_id(run_id, &active)?;
        let report = session_outcome_report(&outcome).clone();
        self.apply_session_report(run_id, &step_id, &report, active.session.session_id())?;
        self.apply_session_markers(run_id, &report.turn_text)?;
        if matches!(
            outcome,
            SessionOutcome::Completed(_) | SessionOutcome::Running(_) | SessionOutcome::Waiting(_)
        ) && let Some(message) = self
            .get_run(run_id)
            .and_then(|run| run.queued_messages.as_ref())
            .and_then(|messages| messages.first())
            .cloned()
        {
            return self.begin_queued_message_turn(run_id, active, message);
        }
        let refresh_prompt = if self.intelligent_context_refresh {
            report.plan_entries.as_deref().and_then(|entries| {
                context_refresh::observe_plan(&mut active.plan_checkpoint, entries, true)
            })
        } else {
            None
        };
        if let Some(refresh_prompt) = refresh_prompt
            && matches!(
                &outcome,
                SessionOutcome::Completed(_) | SessionOutcome::Waiting(_)
            )
        {
            self.update_step(
                run_id,
                &step_id,
                StepPatch::new()
                    .set("status", StepStatus::Pending)
                    .clear("finishedAt")
                    .clear("error"),
            )?;
            self.update_run(
                run_id,
                RunPatch::new()
                    .set("status", RunStatus::Queued)
                    .clear("currentStepId")
                    .clear("finishedAt"),
            )?;
            self.release_workspace_hold(run_id);
            self.plan_checkpoints
                .insert(run_id.to_owned(), active.plan_checkpoint);
            self.pending_context_prompts
                .insert(run_id.to_owned(), refresh_prompt);
            self.jobs.insert(
                run_id.to_owned(),
                RuntimeJob::Workflow {
                    workflow: active.workflow,
                    start_index: active.step_index,
                    retry_counts: active.retry_counts,
                },
            );
            self.enqueue(run_id.to_owned());
            self.append_event(
                run_id,
                EventInput::new("note").field(
                    "message",
                    "intelligent context refresh — reopening a fresh session",
                ),
            )?;
            self.in_flight.remove(run_id);
            self.pump()?;
            return Ok(TurnStep::Done);
        }
        let should_nudge = self
            .get_run(run_id)
            .is_some_and(|run| run.autonomous == Some(true))
            && matches!(
                &outcome,
                SessionOutcome::Waiting(report)
                    if report.decision.is_none()
                        || report.decision == Some(TurnMarkerDecision::Idle)
            )
            && active.auto_continues < MAX_AUTONOMOUS_CONTINUES;
        if should_nudge {
            active.auto_continues += 1;
            self.append_event(
                run_id,
                EventInput::new("note").field(
                    "message",
                    format!(
                        "autonomous pass {} of {}",
                        active.auto_continues, MAX_AUTONOMOUS_CONTINUES
                    ),
                ),
            )?;
            return Ok(TurnStep::Nudge(Box::new(active)));
        }
        match outcome {
            SessionOutcome::Failed { message, .. } => {
                self.in_flight.remove(run_id);
                if let Some(failover) = active.failover.clone()
                    && self.try_auto_failover(
                        run_id,
                        &step_id,
                        failover.concrete,
                        &message,
                        false,
                    )?
                {
                    self.requeue_for_retry(
                        run_id,
                        active.workflow,
                        active.step_index,
                        active.retry_counts,
                        failover.retry_prompt,
                    )?;
                    return Ok(TurnStep::Done);
                }
                self.fail_run(run_id, Some(&step_id), message)?;
                Ok(TurnStep::Done)
            }
            SessionOutcome::Cancelled(_) => {
                self.in_flight.remove(run_id);
                self.cancel_run_after_session(run_id, &step_id)?;
                Ok(TurnStep::Done)
            }
            SessionOutcome::Running(_) => {
                self.in_flight.remove(run_id);
                active.holds_slot = true;
                self.park_session(run_id, active, None)?;
                Ok(TurnStep::Done)
            }
            SessionOutcome::Waiting(report) => {
                self.in_flight.remove(run_id);
                active.holds_slot = false;
                self.park_session(run_id, active, report.decision)?;
                Ok(TurnStep::Done)
            }
            SessionOutcome::Completed(_) => {
                self.in_flight.remove(run_id);
                self.complete_step(run_id, &step_id, None)?;
                if active.next_index < active.workflow.steps.len() {
                    self.update_run(
                        run_id,
                        RunPatch::new()
                            .set("status", RunStatus::Queued)
                            .clear("finishedAt")
                            .clear("currentStepId"),
                    )?;
                    self.jobs.insert(
                        run_id.to_owned(),
                        RuntimeJob::Workflow {
                            workflow: active.workflow,
                            start_index: active.next_index,
                            retry_counts: active.retry_counts,
                        },
                    );
                    self.enqueue(run_id.to_owned());
                } else {
                    if self.should_prepare_git_auto_commit(run_id) {
                        self.append_event(
                            run_id,
                            EventInput::new("note")
                                .field("message", "preparing automatic commit message"),
                        )?;
                        return Ok(TurnStep::GitAutoCommit(Box::new(active)));
                    }
                    self.settle_success(run_id)?;
                }
                self.pump()?;
                Ok(TurnStep::Done)
            }
        }
    }

    pub(super) fn should_prepare_git_auto_commit(&mut self, run_id: &str) -> bool {
        let Some(run) = self.get_run(run_id).cloned() else {
            return false;
        };
        run.git_auto == Some(true)
            && self
                .diff_inspector
                .as_mut()
                .is_some_and(|inspector| inspector.has_diff(&run))
    }

    /// Record the synthetic commit-subject turn and return its subject to the production
    /// dispatcher. The caller must then either call [`Self::finish_git_auto`] after its Git work
    /// or use the returned error as that method's failure reason.
    pub fn apply_git_auto_commit_message(
        &mut self,
        run_id: &str,
        active: RuntimeActive,
        outcome: Result<SessionOutcome, String>,
    ) -> io::Result<Result<GitAutoMessage, String>> {
        self.in_flight.remove(run_id);
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(message) => return Ok(Err(message)),
        };
        let report = session_outcome_report(&outcome).clone();
        let step_id = self.checked_active_step_id(run_id, &active)?;
        self.apply_session_report(run_id, &step_id, &report, active.session.session_id())?;
        self.apply_session_markers(run_id, &report.turn_text)?;
        match outcome {
            SessionOutcome::Cancelled(_) => {
                self.cancel_run_after_session(run_id, &step_id)?;
                Ok(Ok(GitAutoMessage::Cancelled))
            }
            SessionOutcome::Failed { message, .. } => Ok(Err(message)),
            SessionOutcome::Completed(_)
            | SessionOutcome::Running(_)
            | SessionOutcome::Waiting(_) => {
                Ok(commit_subject(&report.turn_text).map(GitAutoMessage::Subject))
            }
        }
    }

    /// Settle the Git action that follows a successful automatic commit-subject turn. A failure
    /// deliberately leaves the completed changes in Review so the user can inspect, commit, or
    /// push them manually instead of losing a successful agent run to a Git configuration issue.
    pub fn finish_git_auto(&mut self, run_id: &str, result: Result<(), String>) -> io::Result<()> {
        match result {
            Ok(()) => {
                self.update_run(
                    run_id,
                    RunPatch::new()
                        .set("status", RunStatus::Done)
                        .set("finishedAt", now_iso8601())
                        .clear("currentStepId")
                        .clear("autoResumeAttempts"),
                )?;
                self.append_event(
                    run_id,
                    EventInput::new("lifecycle")
                        .field("message", "automatic commit and push finished"),
                )?;
                self.cleanup_runtime(run_id);
                Ok(())
            }
            Err(reason) => {
                self.update_run(
                    run_id,
                    RunPatch::new()
                        .set("status", RunStatus::Review)
                        .set("finishedAt", now_iso8601())
                        .clear("currentStepId")
                        .clear("autoResumeAttempts"),
                )?;
                self.append_event(
                    run_id,
                    EventInput::new("note").field(
                        "message",
                        format!(
                            "automatic commit/push failed — review and finish manually: {reason}"
                        ),
                    ),
                )?;
                self.cleanup_runtime(run_id);
                Ok(())
            }
        }
    }

    /// The worktree is preferred; single-worktree runs use the manager's repository root.
    pub fn working_directory_for(&self, run: &RunRecord) -> Option<PathBuf> {
        run.worktree_path
            .as_deref()
            .map(PathBuf::from)
            .or_else(|| self.repo_root.clone())
    }

    pub(super) fn announce_auto_route(
        &mut self,
        run_id: &str,
        runner: Runner,
        model: Option<&str>,
    ) -> io::Result<()> {
        if self.auto_routes.contains_key(run_id) {
            let model = model.unwrap_or("provider default");
            self.append_event(
                run_id,
                EventInput::new("note").field(
                    "message",
                    format!(
                        "Auto routing · trying {} · model {model}",
                        runner_label(runner)
                    ),
                ),
            )?;
        }
        Ok(())
    }

    /// Retire a provider that rejected an Auto request before useful work could complete and
    /// select the next engine-ranked candidate. Explicit runner requests never enter this path.
    pub(super) fn try_auto_failover(
        &mut self,
        run_id: &str,
        step_id: &str,
        failed_runner: Runner,
        error: &str,
        opening_failed: bool,
    ) -> io::Result<bool> {
        if self.get_run(run_id).and_then(|run| run.requested_runner) != Some(RunnerSelection::Auto)
            || (!opening_failed && !is_auto_route_failure(error))
        {
            return Ok(false);
        }
        let next = self.auto_routes.get_mut(run_id).and_then(|candidates| {
            candidates.retain(|candidate| *candidate != failed_runner);
            candidates.first().copied()
        });
        let Some(next) = next else {
            if self.auto_routes.contains_key(run_id) {
                self.append_event(
                    run_id,
                    EventInput::new("note")
                        .field("noteKind", "provider-switch")
                        .field("message", "Auto routing · no eligible providers remain"),
                )?;
            }
            return Ok(false);
        };
        self.update_run(run_id, RunPatch::new().set("runner", next).clear("model"))?;
        self.update_step(
            run_id,
            step_id,
            StepPatch::new()
                .set("status", StepStatus::Pending)
                .clear("backend")
                .clear("sessionId")
                .clear("error")
                .clear("finishedAt"),
        )?;
        self.append_event(
            run_id,
            EventInput::new("note")
                .step(step_id.to_owned())
                .field("noteKind", "provider-switch")
                .field(
                    "message",
                    format!(
                        "Auto routing · {} {} — trying {}",
                        runner_label(failed_runner),
                        auto_route_failure_reason(error),
                        runner_label(next)
                    ),
                ),
        )?;
        Ok(true)
    }

    pub(super) fn fail_run(
        &mut self,
        run_id: &str,
        step_id: Option<&str>,
        message: String,
    ) -> io::Result<()> {
        let finished_at = now_iso8601();
        if let Some(step_id) = step_id {
            self.update_step(
                run_id,
                step_id,
                StepPatch::new()
                    .set("status", StepStatus::Failed)
                    .set("error", message.clone())
                    .set("finishedAt", finished_at.clone()),
            )?;
        }
        self.update_run(
            run_id,
            RunPatch::new()
                .set("status", RunStatus::Failed)
                .set("error", message.clone())
                .set("finishedAt", finished_at)
                .clear("currentStepId"),
        )?;
        self.append_event(
            run_id,
            EventInput::new("lifecycle").field("message", format!("run failed — {message}")),
        )?;
        self.cleanup_runtime(run_id);
        Ok(())
    }

    pub(super) fn checked_active_step_id(
        &mut self,
        run_id: &str,
        active: &RuntimeActive,
    ) -> io::Result<String> {
        match active.step_id() {
            Ok(step_id) => Ok(step_id.to_owned()),
            Err(error) => {
                self.fail_invalid_runtime(run_id, &error)?;
                Err(error)
            }
        }
    }

    /// Settle a detached turn whose workflow cursor no longer points at a real step. This is a
    /// corruption boundary: callers retain ownership of the session so it can be dropped only
    /// after their manager guard has been released.
    pub fn fail_invalid_runtime(&mut self, run_id: &str, error: &io::Error) -> io::Result<()> {
        self.in_flight.remove(run_id);
        self.fail_run(run_id, None, error.to_string())?;
        self.pump()
    }

    pub(super) fn cancel_run_after_session(
        &mut self,
        run_id: &str,
        step_id: &str,
    ) -> io::Result<()> {
        let finished_at = now_iso8601();
        self.update_step(
            run_id,
            step_id,
            StepPatch::new()
                .set("status", StepStatus::Cancelled)
                .set("finishedAt", finished_at.clone()),
        )?;
        self.update_run(
            run_id,
            RunPatch::new()
                .set("status", RunStatus::Cancelled)
                .set("finishedAt", finished_at)
                .clear("currentStepId"),
        )?;
        self.append_event(
            run_id,
            EventInput::new("lifecycle").field("message", "run cancelled"),
        )?;
        self.cleanup_runtime(run_id);
        Ok(())
    }

    pub(super) fn cleanup_runtime(&mut self, run_id: &str) {
        self.queue.remove(run_id);
        self.jobs.remove(run_id);
        self.active.remove(run_id);
        self.usage.remove(run_id);
        self.plan_checkpoints.remove(run_id);
        self.pending_context_prompts.remove(run_id);
        self.auto_routes.remove(run_id);
        self.event_appenders.remove(run_id);
        self.release_workspace_hold(run_id);
        self.release_repository_hold(run_id);
    }

    pub(super) fn settle_success(&mut self, run_id: &str) -> io::Result<()> {
        let Some(run) = self.get_run(run_id).cloned() else {
            return Ok(());
        };
        let has_diff = self
            .diff_inspector
            .as_mut()
            .is_some_and(|inspector| inspector.has_diff(&run));
        let status = success_status(
            has_diff,
            self.runtime_options.review_gate,
            run.autonomous == Some(true),
        );
        self.update_run(
            run_id,
            RunPatch::new()
                .set("status", status)
                .set("finishedAt", now_iso8601())
                .clear("currentStepId")
                .clear("autoResumeAttempts"),
        )?;
        self.append_event(
            run_id,
            EventInput::new("lifecycle").field(
                "message",
                if status == RunStatus::Review {
                    "changes ready for review — send feedback, open a draft PR, or finish"
                } else {
                    "run finished"
                },
            ),
        )?;
        self.cleanup_runtime(run_id);
        Ok(())
    }

    /// Cancel queued work, an injected active session, or a live-status record
    /// with no process-local runtime (a run loaded from disk after a restart).
    /// Terminal cleanup removes all process-local queue/job/usage state while
    /// leaving the durable record as the source of truth.
    pub fn cancel(&mut self, run_id: &str) -> io::Result<bool> {
        // A worker already owns this run's session outside any lock this call holds; settling it
        // here would race the worker's own eventual `apply_open_failure`/`apply_admitted_turn`
        // report. Cancellation itself goes through the run's `CancellationToken`, not this call —
        // the caller signals that independently before ever reaching the manager lock. Once the
        // worker observes it, it reports back through the normal `Cancelled` outcome path.
        if self.in_flight.contains(run_id) {
            return Ok(true);
        }
        if self.queue.is_queued(run_id) || self.jobs.contains_key(run_id) {
            self.cleanup_runtime(run_id);
            if self.get_run(run_id).is_none() {
                return Ok(false);
            }
            let finished_at = now_iso8601();
            self.update_run(
                run_id,
                RunPatch::new()
                    .set("status", RunStatus::Cancelled)
                    .set("finishedAt", finished_at)
                    .clear("currentStepId"),
            )?;
            self.append_event(
                run_id,
                EventInput::new("lifecycle").field("message", "cancelled while queued"),
            )?;
            self.pump()?;
            return Ok(true);
        }
        let Some(mut active) = self.active.remove(run_id) else {
            if self.get_run(run_id).is_some_and(|run| {
                matches!(
                    run.status,
                    RunStatus::Queued | RunStatus::Running | RunStatus::Idle | RunStatus::Waiting
                )
            }) {
                self.settle_steps(run_id, StepStatus::Cancelled)?;
                let finished_at = now_iso8601();
                self.update_run(
                    run_id,
                    RunPatch::new()
                        .set("status", RunStatus::Cancelled)
                        .set("finishedAt", finished_at)
                        .clear("currentStepId"),
                )?;
                self.append_event(
                    run_id,
                    EventInput::new("lifecycle").field("message", "run cancelled"),
                )?;
                self.cleanup_runtime(run_id);
                return Ok(true);
            }
            return Ok(false);
        };
        active.session.cancel();
        let step_id = self.checked_active_step_id(run_id, &active)?;
        self.cancel_run_after_session(run_id, &step_id)?;
        self.pump()?;
        Ok(true)
    }

    /// Settle every in-flight step of a run, so a settled record has no dangling
    /// live steps.
    pub(super) fn settle_steps(&mut self, run_id: &str, status: StepStatus) -> io::Result<()> {
        let finished_at = now_iso8601();
        let Some(run) = self.get_run(run_id).cloned() else {
            return Ok(());
        };
        for step in &run.steps {
            if matches!(
                step.status,
                StepStatus::Pending | StepStatus::Waiting | StepStatus::Running
            ) {
                self.update_step(
                    run_id,
                    &step.id,
                    StepPatch::new()
                        .set("status", status)
                        .set("finishedAt", finished_at.clone()),
                )?;
            }
        }
        Ok(())
    }

    /// Begin finishing a parked session without calling the provider. The returned live session
    /// is marked in-flight and must be driven outside the manager lock, then returned through
    /// [`Self::apply_finish_turn`].
    pub fn begin_finish(&mut self, run_id: &str) -> io::Result<FinishStart> {
        let Some(active) = self.active.remove(run_id) else {
            if self
                .get_run(run_id)
                .is_some_and(|run| run.status == RunStatus::Review)
            {
                self.update_run(run_id, RunPatch::new().set("status", RunStatus::Done))?;
                self.append_event(
                    run_id,
                    EventInput::new("lifecycle")
                        .field("message", "review accepted — finished without a PR"),
                )?;
                self.cleanup_runtime(run_id);
                self.pump()?;
                return Ok(FinishStart::Finished(true));
            }
            if self
                .get_run(run_id)
                .is_some_and(|run| matches!(run.status, RunStatus::Idle | RunStatus::Waiting))
            {
                self.settle_steps(run_id, StepStatus::Done)?;
                self.settle_success(run_id)?;
                self.pump()?;
                return Ok(FinishStart::Finished(true));
            }
            return Ok(FinishStart::Finished(false));
        };
        self.in_flight.insert(run_id.to_owned());
        Ok(FinishStart::Detached(Box::new(active)))
    }

    /// Apply the result of a detached user-requested finish. Waiting/running provider outcomes
    /// are coerced to completion because the user explicitly closed the session.
    pub fn apply_finish_turn(
        &mut self,
        run_id: &str,
        active: RuntimeActive,
        finish_result: Result<SessionOutcome, String>,
    ) -> io::Result<TurnStep> {
        let outcome = match finish_result {
            Ok(outcome) => outcome,
            Err(error) => {
                self.in_flight.remove(run_id);
                self.active.insert(run_id.to_owned(), active);
                self.append_event(
                    run_id,
                    EventInput::new("error")
                        .field("message", format!("could not finish session: {error}")),
                )?;
                return Ok(TurnStep::Done);
            }
        };
        if let Err(error) = self.append_event(
            run_id,
            EventInput::new("lifecycle").field("message", "session closed by user"),
        ) {
            self.in_flight.remove(run_id);
            self.active.insert(run_id.to_owned(), active);
            return Err(error);
        }
        let outcome = match outcome {
            SessionOutcome::Running(report) | SessionOutcome::Waiting(report) => {
                SessionOutcome::Completed(report)
            }
            other => other,
        };
        self.apply_active_turn(run_id, active, Ok(outcome), false)
    }

    #[cfg(test)]
    pub fn finish(&mut self, run_id: &str) -> io::Result<bool> {
        let mut active = match self.begin_finish(run_id)? {
            FinishStart::Finished(finished) => return Ok(finished),
            FinishStart::Detached(active) => *active,
        };
        let step_id = active.step_id()?.to_owned();
        let result = active
            .session_mut()
            .finish(&mut self.event_sink(run_id, &step_id));
        let mut step = self.apply_finish_turn(run_id, active, result)?;
        loop {
            match step {
                TurnStep::Done => return Ok(true),
                TurnStep::GitAutoCommit(_) => {
                    self.finish_git_auto(
                        run_id,
                        Err(
                            "automatic Git actions require the production engine dispatcher"
                                .to_owned(),
                        ),
                    )?;
                    return Ok(true);
                }
                TurnStep::Nudge(mut active) => {
                    let step_id = active.step_id()?.to_owned();
                    let result = active.session_mut().send_message(
                        AUTONOMOUS_NUDGE,
                        &[],
                        &mut self.event_sink(run_id, &step_id),
                    );
                    step = self.apply_active_turn(run_id, *active, result, false)?;
                }
                TurnStep::QueuedMessage {
                    mut active,
                    prompt,
                    images,
                } => {
                    let step_id = active.step_id()?.to_owned();
                    let result = active.session_mut().send_message(
                        &prompt,
                        &images,
                        &mut self.event_sink(run_id, &step_id),
                    );
                    step = self.apply_message_turn(run_id, *active, result, false)?;
                }
            }
        }
    }

    /// Drive a synchronously resumed session in core unit tests through
    /// `continue_active_turn` to completion, sending any autonomous nudge inline under the same
    /// test-owned manager borrow. Production never compiles this path; it uses `TurnDispatch`.
    #[cfg(test)]
    pub(super) fn drive_active_turn(
        &mut self,
        run_id: &str,
        active: RuntimeActive,
        outcome: SessionOutcome,
    ) -> io::Result<()> {
        let mut step = self.continue_active_turn(run_id, active, outcome)?;
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
                        run_id,
                        Err(
                            "automatic Git actions require the production engine dispatcher"
                                .to_owned(),
                        ),
                    )?;
                    break;
                }
                TurnStep::Done => break,
            };
            let step_id = self.checked_active_step_id(run_id, &active)?;
            let result = active.session_mut().send_message(
                &prompt,
                &images,
                &mut self.event_sink(run_id, &step_id),
            );
            step = if queued {
                self.apply_message_turn(run_id, *active, result, false)?
            } else {
                self.apply_active_turn(run_id, *active, result, false)?
            };
        }
        Ok(())
    }

    /// Synchronous convenience used only by core unit tests. Production detaches through
    /// [`Self::begin_message`] and drives the provider on `TurnDispatch`.
    #[cfg(test)]
    pub fn send_message(&mut self, run_id: &str, prompt: impl Into<String>) -> io::Result<bool> {
        self.deliver_message(run_id, prompt, Vec::new())
    }

    /// Synchronous backend-neutral delivery seam used only by core unit tests.
    #[cfg(test)]
    pub fn deliver_message(
        &mut self,
        run_id: &str,
        prompt: impl Into<String>,
        images: Vec<PromptImage>,
    ) -> io::Result<bool> {
        let prompt = prompt.into();
        let Some(active) = self.active.get(run_id) else {
            return Ok(false);
        };
        let step_id = active.step_id().map(str::to_owned);
        let step_id = match step_id {
            Ok(step_id) => step_id,
            Err(error) => {
                self.fail_invalid_runtime(run_id, &error)?;
                return Ok(false);
            }
        };
        self.append_user_message(run_id, &step_id, &prompt, &images)?;
        let Some(mut active) = self.active.remove(run_id) else {
            return Ok(false);
        };
        let send_result =
            active
                .session
                .send_message(&prompt, &images, &mut self.event_sink(run_id, &step_id));
        let outcome = match send_result {
            Ok(outcome) => outcome,
            Err(_) => {
                self.active.insert(run_id.to_owned(), active);
                return Ok(false);
            }
        };
        self.drive_active_turn(run_id, active, outcome)?;
        Ok(true)
    }

    /// Durably append a follow-up and detach its parked session without calling the provider.
    /// The caller owns the returned session until it reports the outcome through
    /// [`Self::apply_active_turn`]. No active session means `continue_run` is the valid path.
    pub fn begin_message(
        &mut self,
        run_id: &str,
        prompt: impl Into<String>,
        images: Vec<PromptImage>,
    ) -> io::Result<Option<RuntimeActive>> {
        let prompt = prompt.into();
        let Some(active) = self.active.get(run_id) else {
            return Ok(None);
        };
        let step_id = match active.step_id() {
            Ok(step_id) => step_id.to_owned(),
            Err(error) => {
                self.fail_invalid_runtime(run_id, &error)?;
                return Ok(None);
            }
        };
        self.append_user_message(run_id, &step_id, &prompt, &images)?;
        self.update_run(
            run_id,
            RunPatch::new()
                .set("status", RunStatus::Running)
                .clear("activity"),
        )?;
        self.update_step(
            run_id,
            &step_id,
            StepPatch::new().set("status", StepStatus::Running),
        )?;
        let Some(active) = self.active.remove(run_id) else {
            return Ok(None);
        };
        self.in_flight.insert(run_id.to_owned());
        Ok(Some(active))
    }

    /// Persist a follow-up that cannot be delivered until the current turn boundary. Queued and
    /// running are the only honest states for this operation: parked sessions use
    /// [`Self::begin_message`] directly, while terminal runs require `continue_run`.
    pub fn queue_message(
        &mut self,
        run_id: &str,
        prompt: String,
        images: Vec<PromptImage>,
    ) -> io::Result<Option<QueuedMessage>> {
        let Some(run) = self.get_run(run_id) else {
            return Ok(None);
        };
        if !matches!(run.status, RunStatus::Queued | RunStatus::Running) {
            return Ok(None);
        }
        let message = QueuedMessage {
            id: new_queued_message_id(),
            text: prompt,
            images: (!images.is_empty())
                .then(|| images.iter().map(PromptImage::data_url).collect::<Vec<_>>()),
            created_at: now_iso8601(),
        };
        let queued = message.clone();
        self.edit_run(run_id, |record| {
            record
                .queued_messages
                .get_or_insert_with(Vec::new)
                .push(queued);
        })?;
        Ok(Some(message))
    }

    /// Apply a detached user follow-up while preserving the parked session on a provider-level
    /// delivery error. Cancellation still wins and follows the ordinary cancelled-turn path.
    pub fn apply_message_turn(
        &mut self,
        run_id: &str,
        active: RuntimeActive,
        send_result: Result<SessionOutcome, String>,
        cancellation_requested: bool,
    ) -> io::Result<TurnStep> {
        let outcome = match send_result {
            Ok(outcome) => outcome,
            Err(error) if cancellation_requested => {
                return self.apply_active_turn(run_id, active, Err(error), true);
            }
            Err(error) => {
                self.in_flight.remove(run_id);
                let step_id = self.checked_active_step_id(run_id, &active)?;
                self.update_run(run_id, RunPatch::new().set("status", RunStatus::Idle))?;
                self.update_step(
                    run_id,
                    &step_id,
                    StepPatch::new().set("status", StepStatus::Waiting),
                )?;
                self.active.insert(run_id.to_owned(), active);
                self.append_event(
                    run_id,
                    EventInput::new("error")
                        .field("message", format!("could not deliver follow-up: {error}")),
                )?;
                return Ok(TurnStep::Done);
            }
        };
        self.apply_active_turn(run_id, active, Ok(outcome), cancellation_requested)
    }

    /// Put a detached parked session back if the OS could not start its worker. The durable user
    /// message remains in history, and the run remains available for retry or cancellation.
    pub fn abandon_message(&mut self, run_id: &str, active: RuntimeActive) {
        self.in_flight.remove(run_id);
        let running = active.holds_slot;
        let _ = self.update_run(
            run_id,
            RunPatch::new().set(
                "status",
                if running {
                    RunStatus::Running
                } else {
                    RunStatus::Idle
                },
            ),
        );
        let step_id = match active.step_id() {
            Ok(step_id) => step_id.to_owned(),
            Err(error) => {
                let _ = self.fail_invalid_runtime(run_id, &error);
                return;
            }
        };
        let _ = self.update_step(
            run_id,
            &step_id,
            StepPatch::new().set(
                "status",
                if running {
                    StepStatus::Running
                } else {
                    StepStatus::Waiting
                },
            ),
        );
        self.active.insert(run_id.to_owned(), active);
    }

    /// Reattach a detached session whose finish worker could not be created. Finishing does not
    /// change the durable status before dispatch, so no status rollback is required.
    pub fn abandon_finish(&mut self, run_id: &str, active: RuntimeActive) {
        self.in_flight.remove(run_id);
        self.active.insert(run_id.to_owned(), active);
    }

    /// Reopen the last persisted session as a new synthetic step. Overrides are written before
    /// queueing so a later continuation and the cockpit both see the selected runner/model.
    pub fn continue_run(
        &mut self,
        run_id: &str,
        options: ContinueOptions,
    ) -> io::Result<ContinueResult> {
        if self.active.contains_key(run_id) {
            return Ok(ContinueResult::error("run is still active"));
        }
        let Some(run) = self.get_run(run_id).cloned() else {
            return Ok(ContinueResult::error("not found"));
        };
        if !matches!(
            run.status,
            RunStatus::Done | RunStatus::Failed | RunStatus::Cancelled | RunStatus::Review
        ) {
            return Ok(ContinueResult::error(format!(
                "cannot continue a {} run",
                run_status_name(run.status)
            )));
        }
        // A run with no prior session (e.g. the agent crashed before its first turn) still gets
        // a fresh step in this same run/worktree — it just starts without a resumed transcript,
        // the same as a resumed step whose backend no longer matches the target runner below.
        let session_step = run
            .steps
            .iter()
            .rev()
            .find(|step| step.session_id.is_some());
        let target_runner = options
            .runner
            .unwrap_or(run.requested_runner.unwrap_or_else(|| {
                run.runner
                    .map(runner_selection)
                    .unwrap_or(RunnerSelection::Claude)
            }));
        if target_runner == RunnerSelection::Auto
            && options
                .model
                .as_deref()
                .is_some_and(|model| !model.is_empty())
        {
            return Ok(ContinueResult::error(
                "a model override cannot be used with quota-aware routing",
            ));
        }
        let target_concrete = concrete_runner(target_runner);
        if let (Some(model), Some(runner)) = (options.model.as_deref(), target_concrete)
            && model_conflicts_with_runner(model, runner)
        {
            return Ok(ContinueResult::error(format!(
                "model '{model}' is not a {} model",
                runner_name(runner)
            )));
        }
        let Some(workflow) = run.workflow_def.clone() else {
            return Ok(ContinueResult::error(
                "workflow definition not recoverable for continuation",
            ));
        };
        let session_backend = session_step
            .and_then(|step| step.backend)
            .or(run.runner)
            .unwrap_or(Runner::Claude);
        let prior_session_id = session_step.and_then(|step| step.session_id.clone());
        let resume_session = (target_concrete == Some(session_backend))
            .then(|| prior_session_id.clone())
            .flatten();
        // A real prior session exists but the runner switch makes it unresumable: the new step
        // starts with an empty transcript instead of the one the user was just looking at. Say so
        // — silently dropping the conversation is exactly the kind of quiet capability loss
        // CODE_REVIEW.md rules out; the user should see this in the same place they'd see any
        // other run note, not have to notice a step counter or discover it from a confused reply.
        if prior_session_id.is_some() && resume_session.is_none() {
            self.append_event(
                run_id,
                EventInput::new("note").field(
                    "message",
                    format!(
                        "switching from {} to {} starts a fresh session — the previous conversation is not resumed",
                        runner_name(session_backend),
                        runner_name(target_concrete.unwrap_or(Runner::Claude)),
                    ),
                ),
            )?;
        }

        if options.runner.is_some() || options.model.is_some() {
            let mut patch = RunPatch::new();
            if let Some(runner) = options.runner {
                patch = patch.set("requestedRunner", runner);
                if runner != RunnerSelection::Auto {
                    patch = patch.set("runner", runner);
                }
            }
            if let Some(model) = options.model {
                if model.is_empty() {
                    patch = patch.clear("model");
                } else {
                    patch = patch.set("model", model);
                }
            } else if options.runner.is_some()
                && (target_runner == RunnerSelection::Auto
                    || run
                        .model
                        .as_deref()
                        .zip(target_concrete)
                        .is_some_and(|(model, runner)| model_conflicts_with_runner(model, runner)))
            {
                patch = patch.clear("model");
            }
            self.update_run(run_id, patch)?;
        }

        let count = run
            .steps
            .iter()
            .filter(|step| step.id.starts_with("continue-"))
            .count();
        let step_id = format!("continue-{}", count + 1);
        self.add_step(
            run_id,
            StepSeed {
                id: step_id.clone(),
                name: "Continue".to_owned(),
                kind: StepKind::Agent,
                requested_runner: Some(target_runner),
            },
        )?;
        let prompt = options
            .text
            .filter(|text| !text.trim().is_empty())
            .unwrap_or_else(|| {
                if options.images.is_empty() {
                    "Continue.".to_owned()
                } else {
                    String::new()
                }
            });
        self.append_user_message(run_id, &step_id, &prompt, &options.images)?;
        let model = self.get_run(run_id).and_then(|run| run.model.clone());
        let mut continuation_workflow = workflow;
        continuation_workflow
            .steps
            .push(coducktor_contract::workflows::WorkflowStepDef {
                id: step_id.clone(),
                name: Some("Continue".to_owned()),
                prompt: Some("{{task}}".to_owned()),
                skill: None,
                model: None,
                runner: Some(target_runner),
                allowed_tools: None,
                bash_allowlist: None,
                command: None,
                on_fail: None,
            });
        self.update_run(
            run_id,
            RunPatch::new()
                .set("workflowDef", &continuation_workflow)
                .set("status", RunStatus::Queued)
                .clear("error")
                .clear("finishedAt")
                .clear("currentStepId")
                .set("requestedRunner", target_runner),
        )?;
        let step_index = continuation_workflow.steps.len().saturating_sub(1);
        self.jobs.insert(
            run_id.to_owned(),
            RuntimeJob::Continuation {
                workflow: continuation_workflow,
                step_index,
                session_id: resume_session,
                prompt,
                images: options.images,
                runner: target_runner,
                model,
                retry_counts: BTreeMap::new(),
            },
        );
        self.enqueue(run_id.to_owned());
        self.pump()?;
        Ok(ContinueResult::ok())
    }

    pub(super) fn append_user_message(
        &mut self,
        run_id: &str,
        step_id: &str,
        prompt: &str,
        images: &[PromptImage],
    ) -> io::Result<RunEvent> {
        let image_urls = images.iter().map(PromptImage::data_url).collect::<Vec<_>>();
        self.append_event(
            run_id,
            EventInput::new("user-message")
                .step(step_id.to_owned())
                .field("text", prompt)
                .field("imageCount", image_urls.len())
                .field("images", image_urls),
        )
    }

    /// Queued input folded into a not-yet-started prompt still needs its own transcript row.
    /// Append those durable user events immediately before admission, then clear the queue so a
    /// later workflow step cannot fold the same messages into its prompt again.
    pub(super) fn acknowledge_queued_messages(
        &mut self,
        run_id: &str,
        step_id: &str,
    ) -> io::Result<()> {
        let messages = self
            .get_run(run_id)
            .and_then(|run| run.queued_messages.clone())
            .unwrap_or_default();
        if messages.is_empty() {
            return Ok(());
        }
        for message in &messages {
            let images = message
                .images
                .iter()
                .flatten()
                .filter_map(|url| PromptImage::from_data_url(url))
                .collect::<Vec<_>>();
            self.append_user_message(run_id, step_id, &message.text, &images)?;
        }
        self.edit_run(run_id, |record| record.queued_messages = None)?;
        Ok(())
    }

    /// Apply parsed agent-owned PR/issue markers to a record. URL candidate discovery remains a
    /// separate session concern; this method preserves the authoritative marker fields and
    /// resolves any candidate set already present on the record.
    pub fn apply_marker_refs(
        &mut self,
        run_id: &str,
        refs: &TaskMarkers,
    ) -> io::Result<Option<RunRecord>> {
        if refs.pr.is_none() && refs.issue.is_none() {
            return Ok(self.get_run(run_id).cloned());
        }
        let Some(previous) = self.runs.get(run_id).cloned() else {
            return Ok(None);
        };
        let mut next = previous.clone();
        let mut marker_refs = next.marker_refs.take().unwrap_or(MarkerRefs {
            pr: None,
            issue: None,
        });
        if let Some(pr) = refs.pr {
            marker_refs.pr = Some(pr as f64);
            next.pr_number = Some(pr as f64);
            next.referenced_pull_request_url = resolve_reference(
                next.referenced_pr_candidates.as_deref().unwrap_or(&[]),
                &next.task,
                Some(pr),
            );
        }
        if let Some(issue) = refs.issue {
            marker_refs.issue = Some(issue as f64);
            next.issue_number = Some(issue as f64);
            next.referenced_issue_number_seeded = None;
            next.referenced_issue_url = resolve_reference(
                next.referenced_issue_candidates.as_deref().unwrap_or(&[]),
                &next.task,
                Some(issue),
            );
        }
        next.marker_refs = Some(marker_refs);
        self.replace_record(run_id, previous, next).map(Some)
    }

    /// Parse and apply the reference/title markers from one completed turn. This deliberately
    /// reads only the supplied agent text; tool output should call `append_event`, not this helper.
    pub fn apply_turn_markers(
        &mut self,
        run_id: &str,
        turn_text: &str,
    ) -> io::Result<Option<RunRecord>> {
        let markers = task_markers::parse_task_markers(turn_text);
        let _ = self.apply_marker_refs(run_id, &markers)?;
        let Some(current) = self.get_run(run_id).cloned() else {
            return Ok(None);
        };
        let Some(title) = markers.title.as_deref() else {
            return Ok(Some(current));
        };
        if current.title_origin == Some(coducktor_contract::runs::TitleOrigin::User) {
            return Ok(Some(current));
        }
        let ref_number = current
            .pr_number
            .or(current.issue_number)
            .map(|number| number as i64);
        let Some(title) = post_validate_marker_title(title, ref_number) else {
            return Ok(Some(current));
        };
        let patch = RunPatch::new()
            .set("titleSummary", title)
            .set("titleOrigin", coducktor_contract::runs::TitleOrigin::Marker);
        self.update_run(run_id, patch)
    }
}
