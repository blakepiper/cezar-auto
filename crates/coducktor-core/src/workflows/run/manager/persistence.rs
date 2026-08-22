use super::*;

impl RunManager {
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Repository root a session should run in. Managers created by core-only tests can omit it;
    /// production construction always provides it explicitly.
    pub(super) fn repo_root(&self) -> PathBuf {
        self.repo_root
            .clone()
            .unwrap_or_else(|| self.data_dir.clone())
    }

    pub fn set_session_factory(&mut self, session_factory: impl SessionFactory + 'static) {
        self.session_factory = Some(Box::new(session_factory));
    }

    pub fn set_check_executor(&mut self, check_executor: impl CheckExecutor + 'static) {
        self.check_executor = Some(Box::new(check_executor));
    }

    pub fn set_diff_inspector(&mut self, diff_inspector: impl DiffInspector + 'static) {
        self.diff_inspector = Some(Box::new(diff_inspector));
    }

    pub fn set_runtime_options(&mut self, options: RuntimeOptions) {
        self.runtime_options = options;
    }

    pub fn set_project_id(&mut self, project_id: impl Into<String>) {
        self.project_id = project_id.into();
    }

    pub fn set_workspace_semaphore(&mut self, semaphore: impl WorkspaceSemaphore + 'static) {
        self.workspace_semaphore = Some(Box::new(semaphore));
    }

    pub fn set_repository_lease(&mut self, lease: impl RepositoryRootLease + 'static) {
        self.repository_lease = Some(Box::new(lease));
    }

    pub fn set_intelligent_context_refresh(&mut self, enabled: bool) {
        self.intelligent_context_refresh = enabled;
    }

    pub fn runtime_options(&self) -> RuntimeOptions {
        self.runtime_options
    }

    pub fn runtime_metrics(&self) -> RuntimeMetrics {
        RuntimeMetrics {
            event_appends: self.event_append_count,
            index_flushes: self.index_write_count,
            index_flush_bytes: self.index_write_bytes,
            active_sessions: self.active.len(),
            queued_jobs: self.jobs.len(),
        }
    }

    pub fn get_run(&self, run_id: &str) -> Option<&RunRecord> {
        self.runs.get(run_id)
    }

    pub fn list_runs(&self) -> Vec<RunRecord> {
        let records: Vec<RunRecord> = self.runs.values().cloned().collect();
        store::list_runs_by_recency(&records)
            .into_iter()
            .cloned()
            .collect()
    }

    /// Remove a run and its append-only event log. Callers must check that the run is not active.
    pub fn remove_run(&mut self, run_id: &str) -> io::Result<bool> {
        let Some(run) = self.runs.remove(run_id) else {
            return Ok(false);
        };
        self.cleanup_runtime(run_id);
        if let Err(error) = self.persist() {
            self.runs.insert(run_id.to_owned(), run);
            return Err(error);
        }
        let event_path = events::events_path(&self.data_dir, run_id);
        let _ = fs::remove_file(event_path);
        crate::handoff::delete_handoff(&self.data_dir, run_id);
        Ok(true)
    }

    /// Remove terminal records beyond the durable index retention budget. The replacement index
    /// is committed before best-effort sidecar cleanup, so a crash cannot leave an index entry
    /// that points at already-deleted history. Queued, running, and waiting work is never a
    /// retention candidate, even if an old clock or imported state makes it look stale.
    ///
    /// Worktrees have their own explicit retention policy: removing an index record must not
    /// remove a checkout that may contain recoverable agent edits.
    pub fn prune_stale_runs(&mut self) -> io::Result<Vec<String>> {
        let candidates = store::select_stale_run_ids(&self.list_runs());
        let stale: Vec<String> = candidates
            .into_iter()
            .filter(|run_id| {
                self.runs.get(run_id).is_some_and(|run| {
                    matches!(
                        run.status,
                        RunStatus::Done
                            | RunStatus::Failed
                            | RunStatus::Cancelled
                            | RunStatus::Review
                    ) && !self.is_active(run_id)
                })
            })
            .collect();
        if stale.is_empty() {
            return Ok(stale);
        }

        let removed: Vec<(String, RunRecord)> = stale
            .iter()
            .filter_map(|run_id| self.runs.remove(run_id).map(|run| (run_id.clone(), run)))
            .collect();
        if let Err(error) = self.persist() {
            self.runs.extend(removed);
            return Err(error);
        }
        for run_id in &stale {
            let _ = fs::remove_file(events::events_path(&self.data_dir, run_id));
            crate::handoff::delete_handoff(&self.data_dir, run_id);
        }
        Ok(stale)
    }

    /// Register an observer for appended events. The callback is invoked after the NDJSON append
    /// succeeds and receives an owned notification view, so it cannot mutate manager state by
    /// aliasing a record reference.
    pub fn subscribe_events<F>(&mut self, observer: F) -> EventObserverId
    where
        F: Fn(&RunEventNotification) + Send + Sync + 'static,
    {
        let id = self.next_observer_id();
        self.event_observers.insert(id, Box::new(observer));
        id
    }

    pub fn unsubscribe_events(&mut self, observer_id: EventObserverId) -> bool {
        self.event_observers.remove(&observer_id).is_some()
    }

    /// Register an observer for durable record updates.
    pub fn subscribe_runs<F>(&mut self, observer: F) -> RunObserverId
    where
        F: Fn(&RunRecord) + Send + Sync + 'static,
    {
        let id = self.next_observer_id();
        self.run_observers.insert(id, Box::new(observer));
        id
    }

    pub fn unsubscribe_runs(&mut self, observer_id: RunObserverId) -> bool {
        self.run_observers.remove(&observer_id).is_some()
    }

    pub(super) fn next_observer_id(&mut self) -> u64 {
        self.next_observer_id = self.next_observer_id.wrapping_add(1);
        self.next_observer_id
    }

    pub(super) fn persist(&mut self) -> io::Result<()> {
        if self.write_quarantined {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "runs.json is quarantined because existing state could not be fully loaded",
            ));
        }
        fs::create_dir_all(self.data_dir.join("runs"))?;
        let records: Vec<RunRecord> = self.runs.values().cloned().collect();
        let index_path = store::index_path(&self.data_dir);
        store::write_run_index(&index_path, &records)?;
        self.index_write_count += 1;
        if let Ok(metadata) = fs::metadata(index_path) {
            self.index_write_bytes = self.index_write_bytes.saturating_add(metadata.len());
        }
        self.last_index_flush = Instant::now();
        Ok(())
    }

    /// Flush is kept explicit for callers that want a named shutdown boundary. Mutations are
    /// already written synchronously before they return.
    pub fn flush(&mut self) -> io::Result<()> {
        self.persist()
    }

    /// Repair state that was quarantined during load. This is deliberately explicit: the
    /// original index is backed up before the currently salvaged records replace it.
    pub fn repair_quarantined_index(&mut self) -> io::Result<Option<PathBuf>> {
        if !self.write_quarantined {
            return Ok(None);
        }
        let records: Vec<RunRecord> = self.runs.values().cloned().collect();
        let backup =
            store::backup_then_repair_run_index(&store::index_path(&self.data_dir), &records)?;
        self.write_quarantined = false;
        self.last_index_flush = Instant::now();
        Ok(Some(backup))
    }

    pub(super) fn notify_run(&self, run: &RunRecord) {
        for observer in self.run_observers.values() {
            observer(run);
        }
    }

    pub(super) fn notify_event(&self, notification: &RunEventNotification) {
        for observer in self.event_observers.values() {
            observer(notification);
        }
    }

    pub(super) fn replace_record(
        &mut self,
        run_id: &str,
        previous: RunRecord,
        next: RunRecord,
    ) -> io::Result<RunRecord> {
        self.runs.insert(run_id.to_owned(), next.clone());
        if let Err(error) = self.persist() {
            self.runs.insert(run_id.to_owned(), previous);
            return Err(error);
        }
        self.notify_run(&next);
        Ok(next)
    }

    /// Create and durably persist a queued record.
    pub fn create_run(&mut self, input: CreateRunInput) -> io::Result<RunRecord> {
        let id = new_run_id();
        let created_at = now_iso8601();
        let mut steps: Vec<StepState> = input.steps.into_iter().map(step_from_seed).collect();
        if let (Some(decision), Some(first)) = (input.routing_decision, steps.first_mut()) {
            first.routing_decision = Some(decision);
        }
        let run = RunRecord {
            id: id.clone(),
            title: input.title,
            workflow: input.workflow,
            task: input.task,
            task_images: input.task_images,
            model: input.model,
            reasoning_effort: input.reasoning_effort,
            model_identity: input.model_identity,
            runner: input.runner,
            requested_runner: input.requested_runner,
            agent_profile: input.agent_profile,
            system_prompt: input.system_prompt,
            autonomous: input.autonomous,
            git_auto: input.git_auto,
            worktree: input.worktree,
            group_id: input.group_id,
            variant: input.variant,
            status: RunStatus::Queued,
            created_at: created_at.clone(),
            updated_at: Some(created_at),
            tokens_used: 0.0,
            archived: false,
            steps,
            workflow_def: input.workflow_def,
            ..RunRecord::default()
        };
        self.runs.insert(id.clone(), run.clone());
        if let Err(error) = self.persist() {
            self.runs.remove(&id);
            return Err(error);
        }
        self.notify_run(&run);
        Ok(run)
    }

    pub fn create_workflow_run(
        &mut self,
        workflow: &WorkflowDef,
        task: impl Into<String>,
    ) -> io::Result<RunRecord> {
        self.create_run(CreateRunInput::from_workflow(workflow, task))
    }

    /// Create and queue one workflow without running it.
    ///
    /// Interactive clients use this accepted-first boundary to obtain the durable run id and
    /// subscribe to its event stream before a potentially long-running agent turn begins.
    pub fn enqueue_run(
        &mut self,
        workflow: &WorkflowDef,
        input: StartRunInput,
    ) -> io::Result<RunRecord> {
        let mut create = CreateRunInput::from_workflow(workflow, input.task.clone());
        create.model = input.model;
        create.reasoning_effort = input.reasoning_effort;
        create.runner = input
            .resolved_runner
            .or_else(|| input.runner.and_then(concrete_runner));
        create.requested_runner = input.runner;
        create.agent_profile = input.agent_profile;
        create.system_prompt = input.system_prompt;
        create.autonomous = input.autonomous;
        create.git_auto = input.git_auto;
        create.worktree = input.worktree;
        create.task_images = (!input.images.is_empty())
            .then(|| input.images.iter().map(PromptImage::data_url).collect());
        create.routing_decision = input.routing_decision;
        let run = self.create_run(create)?;
        if let Some(decision) = run
            .steps
            .first()
            .and_then(|step| step.routing_decision.as_ref())
        {
            self.append_event(
                &run.id,
                EventInput::new("note").field("message", routing_decision_note(decision)),
            )?;
            self.append_event(
                &run.id,
                EventInput::new("routing-decision").field("decision", decision),
            )?;
        }
        if input.runner == Some(RunnerSelection::Auto) {
            self.auto_routes
                .insert(run.id.clone(), input.auto_runner_candidates);
        }
        self.jobs.insert(
            run.id.clone(),
            RuntimeJob::Workflow {
                workflow: workflow.clone(),
                start_index: 0,
                retry_counts: BTreeMap::new(),
            },
        );
        self.enqueue(run.id.clone());
        Ok(run)
    }

    /// Create, queue, and pump one workflow. The returned record is the current durable state,
    /// which may already be terminal when the injected session completes synchronously.
    pub fn start_run(
        &mut self,
        workflow: &WorkflowDef,
        input: StartRunInput,
    ) -> io::Result<RunRecord> {
        let run = self.enqueue_run(workflow, input)?;
        self.run_to_completion()?;
        Ok(self.get_run(&run.id).cloned().unwrap_or(run))
    }

    /// Start up to the three built-in variants in one queue pass. Variant B/C receive the same
    /// fixed diversification hints while the runtime still treats them as ordinary queued jobs.
    pub fn enqueue_variants(
        &mut self,
        workflow: &WorkflowDef,
        input: StartRunInput,
        count: usize,
    ) -> io::Result<Vec<RunRecord>> {
        let group_id = new_run_id();
        let metadata = variants::variant_metadata(&group_id, &input.task, count);
        let mut ids = Vec::with_capacity(count);
        for variant in metadata {
            let mut variant_input = input.clone();
            variant_input.task = variant.task;
            let mut create = CreateRunInput::from_workflow(workflow, variant_input.task.clone());
            create.title = format!("{} ({})", create.title, variant.variant);
            create.model = variant_input.model;
            create.reasoning_effort = variant_input.reasoning_effort;
            create.runner = variant_input
                .resolved_runner
                .or_else(|| variant_input.runner.and_then(concrete_runner));
            create.requested_runner = variant_input.runner;
            create.agent_profile = variant_input.agent_profile;
            create.system_prompt = variant_input.system_prompt;
            create.autonomous = variant_input.autonomous;
            create.worktree = (!variant.isolated).then_some(false);
            create.group_id = Some(group_id.clone());
            create.variant = Some(variant.variant);
            create.task_images = (!variant_input.images.is_empty()).then(|| {
                variant_input
                    .images
                    .iter()
                    .map(PromptImage::data_url)
                    .collect()
            });
            let run = self.create_run(create)?;
            if variant_input.runner == Some(RunnerSelection::Auto) {
                self.auto_routes
                    .insert(run.id.clone(), variant_input.auto_runner_candidates);
            }
            ids.push(run.id.clone());
            self.jobs.insert(
                run.id.clone(),
                RuntimeJob::Workflow {
                    workflow: workflow.clone(),
                    start_index: 0,
                    retry_counts: BTreeMap::new(),
                },
            );
            self.enqueue(run.id);
        }
        Ok(ids
            .into_iter()
            .filter_map(|run_id| self.get_run(&run_id).cloned())
            .collect())
    }

    pub fn start_variants(
        &mut self,
        workflow: &WorkflowDef,
        input: StartRunInput,
        count: usize,
    ) -> io::Result<Vec<RunRecord>> {
        let runs = self.enqueue_variants(workflow, input, count)?;
        self.run_to_completion()?;
        Ok(runs
            .into_iter()
            .map(|run| self.get_run(&run.id).cloned().unwrap_or(run))
            .collect())
    }

    /// Apply a durable contract-shaped patch. Unknown keys are ignored by the shared serde
    /// contract, while wrong values fail before the old record is replaced.
    pub fn update_run(&mut self, run_id: &str, patch: RunPatch) -> io::Result<Option<RunRecord>> {
        let Some(previous) = self.runs.get(run_id).cloned() else {
            return Ok(None);
        };
        let mut next = apply_run_patch(&previous, patch.fields())?;
        next.updated_at = Some(now_iso8601());
        self.replace_record(run_id, previous, next).map(Some)
    }

    pub fn update_run_value(
        &mut self,
        run_id: &str,
        patch: Value,
    ) -> io::Result<Option<RunRecord>> {
        let patch = RunPatch::from_value(patch)
            .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))?;
        self.update_run(run_id, patch)
    }

    /// Escape hatch for lifecycle code that already has a typed `RunRecord` mutation. The
    /// resulting value still goes through the same durable replace/rollback path.
    pub fn edit_run<F>(&mut self, run_id: &str, edit: F) -> io::Result<Option<RunRecord>>
    where
        F: FnOnce(&mut RunRecord),
    {
        let Some(previous) = self.runs.get(run_id).cloned() else {
            return Ok(None);
        };
        let mut next = previous.clone();
        edit(&mut next);
        next.updated_at = Some(now_iso8601());
        self.replace_record(run_id, previous, next).map(Some)
    }

    /// Append a step once. Duplicate ids are a no-op, matching `RunStore.addStep`.
    pub fn add_step(&mut self, run_id: &str, step: StepSeed) -> io::Result<bool> {
        let Some(previous) = self.runs.get(run_id).cloned() else {
            return Ok(false);
        };
        if previous
            .steps
            .iter()
            .any(|candidate| candidate.id == step.id)
        {
            return Ok(false);
        }
        let mut next = previous.clone();
        next.steps.push(step_from_seed(step));
        next.updated_at = Some(now_iso8601());
        self.replace_record(run_id, previous, next)?;
        Ok(true)
    }

    /// Update a step and recompute all run-level usage and cost aggregates.
    pub fn update_step(
        &mut self,
        run_id: &str,
        step_id: &str,
        patch: StepPatch,
    ) -> io::Result<bool> {
        let Some(previous) = self.runs.get(run_id).cloned() else {
            return Ok(false);
        };
        let Some(step_index) = previous.steps.iter().position(|step| step.id == step_id) else {
            return Ok(false);
        };
        let mut next = previous.clone();
        apply_step_patch(&mut next, step_index, patch.fields())?;
        recompute_aggregates(&mut next);
        self.replace_record(run_id, previous, next)?;
        Ok(true)
    }

    pub fn edit_step<F>(&mut self, run_id: &str, step_id: &str, edit: F) -> io::Result<bool>
    where
        F: FnOnce(&mut StepState),
    {
        let Some(previous) = self.runs.get(run_id).cloned() else {
            return Ok(false);
        };
        let Some(step_index) = previous.steps.iter().position(|step| step.id == step_id) else {
            return Ok(false);
        };
        let mut next = previous.clone();
        let step = next.steps.get_mut(step_index).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "run step disappeared during edit",
            )
        })?;
        edit(step);
        recompute_aggregates(&mut next);
        self.replace_record(run_id, previous, next)?;
        Ok(true)
    }

    /// Persist the invocation checkpoint before a backend session is launched.
    pub fn begin_usage_invocation(&mut self, run_id: &str, step_id: &str) -> io::Result<bool> {
        let Some(step) = self
            .runs
            .get(run_id)
            .and_then(|run| run.steps.iter().find(|step| step.id == step_id))
        else {
            return Ok(false);
        };
        let epoch = step.usage_invocation_epoch.unwrap_or(0.0) + 1.0;
        let patch = RunPatch::new().set("usageInvocationEpoch", epoch).set(
            "usageInvocationsStarted",
            step.usage_invocations_started.unwrap_or(0.0) + 1.0,
        );
        if !self.update_step(run_id, step_id, patch)? {
            return Ok(false);
        }
        self.usage.insert(
            run_id.to_owned(),
            UsageInvocation {
                step_id: step_id.to_owned(),
                observed: false,
                started_turns: HashSet::new(),
                recorded_turns: HashSet::new(),
            },
        );
        Ok(true)
    }

    /// Record a backend-neutral turn checkpoint, deduplicated inside the current invocation.
    pub fn record_usage_event(&mut self, run_id: &str, event: UsageEvent) -> io::Result<bool> {
        let Some(invocation) = self.usage.get(run_id).cloned() else {
            return Ok(false);
        };
        let Some(step) = self
            .runs
            .get(run_id)
            .and_then(|run| run.steps.iter().find(|step| step.id == invocation.step_id))
        else {
            return Ok(false);
        };
        match event {
            UsageEvent::TurnStarted { turn_id } => {
                if invocation.started_turns.contains(&turn_id) {
                    return Ok(false);
                }
                let first_observed = !invocation.observed;
                let mut patch = RunPatch::new().set(
                    "usageTurnsStarted",
                    step.usage_turns_started.unwrap_or(0.0) + 1.0,
                );
                if first_observed {
                    patch = patch.set(
                        "usageInvocationsObserved",
                        step.usage_invocations_observed.unwrap_or(0.0) + 1.0,
                    );
                }
                if !self.update_step(run_id, &invocation.step_id, patch)? {
                    return Ok(false);
                }
                if let Some(current) = self.usage.get_mut(run_id) {
                    current.started_turns.insert(turn_id);
                    current.observed = true;
                }
                Ok(true)
            }
            UsageEvent::TurnCompleted {
                turn_id,
                input_tokens,
                output_tokens,
            } => {
                let Some(input_tokens) = input_tokens else {
                    return Ok(false);
                };
                let Some(output_tokens) = output_tokens else {
                    return Ok(false);
                };
                if !input_tokens.is_finite()
                    || input_tokens < 0.0
                    || !output_tokens.is_finite()
                    || output_tokens < 0.0
                    || !invocation.started_turns.contains(&turn_id)
                    || invocation.recorded_turns.contains(&turn_id)
                {
                    return Ok(false);
                }
                let patch = RunPatch::new()
                    .set(
                        "inputTokens",
                        step.input_tokens.unwrap_or(0.0) + input_tokens,
                    )
                    .set(
                        "outputTokens",
                        step.output_tokens.unwrap_or(0.0) + output_tokens,
                    )
                    .set(
                        "usageTurnsRecorded",
                        step.usage_turns_recorded.unwrap_or(0.0) + 1.0,
                    );
                if !self.update_step(run_id, &invocation.step_id, patch)? {
                    return Ok(false);
                }
                if let Some(current) = self.usage.get_mut(run_id) {
                    current.recorded_turns.insert(turn_id);
                }
                Ok(true)
            }
        }
    }

    /// Append one event with a manager-owned sequence and timestamp.
    pub fn append_event(&mut self, run_id: &str, input: EventInput) -> io::Result<RunEvent> {
        if !self.runs.contains_key(run_id) {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("unknown run: {run_id}"),
            ));
        }
        let path = events::events_path(&self.data_dir, run_id);
        let seq = self
            .seqs
            .get(run_id)
            .copied()
            .unwrap_or_else(|| events::rehydrate_seq(&path))
            + 1.0;
        let event = RunEvent {
            seq,
            ts: now_iso8601(),
            step_id: input.step_id,
            event_type: input.event_type,
            extra: input.extra,
        };
        if !self.event_appenders.contains_key(run_id) {
            self.event_appenders.insert(
                run_id.to_owned(),
                events::BufferedEventAppender::open(&path)?,
            );
        }
        self.event_appenders
            .get_mut(run_id)
            .ok_or_else(|| io::Error::other("event appender unavailable"))?
            .append(&event)?;
        self.event_append_count += 1;
        // Event append is meaningful activity. Keep read/unread and archive mutations on their
        // separate timestamps by stamping here instead of in the generic record replacement.
        let updated_run = if let Some(run) = self.runs.get_mut(run_id) {
            run.updated_at = Some(event.ts.clone());
            Some(run.clone())
        } else {
            None
        };
        let flush_index = self.last_index_flush.elapsed() >= Duration::from_millis(250);
        if updated_run.is_some() && flush_index {
            self.persist()?;
        }
        self.seqs.insert(run_id.to_owned(), seq);
        if flush_index && let Some(run) = &updated_run {
            self.notify_run(run);
        }
        let notification = RunEventNotification {
            run_id: run_id.to_owned(),
            event: event.clone(),
        };
        self.notify_event(&notification);
        Ok(event)
    }

    pub fn append_event_fields(
        &mut self,
        run_id: &str,
        event_type: impl Into<String>,
        step_id: Option<String>,
        extra: Map<String, Value>,
    ) -> io::Result<RunEvent> {
        self.append_event(
            run_id,
            EventInput {
                event_type: event_type.into(),
                step_id,
                extra,
            },
        )
    }

    /// A per-turn live event sink: a real [`AgentSession`] calls this once per mid-turn event
    /// (each raw process message a backend's UI mapper turns into one, e.g. a `message.updated`
    /// or `tool.call` chunk) so the transcript persists — and broadcasts to live subscribers —
    /// as the agent actually produces it, rather than waiting for the whole turn to finish. This
    /// is what makes `session.turn()`'s event-sink parameter meaningful: without it, only the
    /// aggregated [`SessionReport::turn_text`] would exist, once, after the turn already ended.
    ///
    /// A `text`-typed event is marker-stripped per chunk
    /// (so `DUCK:DONE`/`DUCK:MONITORING`/task markers never flash in the live transcript, matching
    /// [`Self::apply_session_markers`]'s aggregate-side detection) and dropped if that empties it;
    /// every other event type passes through unchanged.
    pub(super) fn event_sink(
        &mut self,
        run_id: &str,
        step_id: &str,
    ) -> impl FnMut(EventInput) -> io::Result<()> + '_ {
        let run_id = run_id.to_owned();
        let step_id = step_id.to_owned();
        move |mut event: EventInput| {
            if event.step_id.is_none() {
                event.step_id = Some(step_id.clone());
            }
            if event.event_type == "text"
                && let Some(text) = event.extra.get("text").and_then(Value::as_str)
            {
                let stripped = strip_turn_marker(&task_markers::strip_task_markers(text));
                if stripped.is_empty() {
                    return Ok(());
                }
                event
                    .extra
                    .insert("text".to_owned(), Value::String(stripped));
            }
            self.append_event(&run_id, event).map(|_| ())
        }
    }

    /// Apply one live event a worker's `AgentSession::turn`/`send_message` produced, outside any
    /// lock, to durable state. A caller holds this manager's lock only for the duration of this
    /// one call, in between reads from the (lock-free) child process — never across the I/O
    /// itself. Same normalization as the synchronous `event_sink` path.
    pub fn apply_turn_event(
        &mut self,
        run_id: &str,
        step_id: &str,
        event: EventInput,
    ) -> io::Result<()> {
        (self.event_sink(run_id, step_id))(event)
    }

    /// Read the raw event history through the shared event reader.
    pub fn read_events(&self, run_id: &str) -> Vec<RunEvent> {
        events::read_events(&events::events_path(&self.data_dir, run_id))
    }

    pub fn set_archived(&mut self, run_id: &str, archived: bool) -> io::Result<Option<RunRecord>> {
        let now = now_iso8601();
        let Some(previous) = self.runs.get(run_id).cloned() else {
            return Ok(None);
        };
        let mut next = previous.clone();
        next.archived = archived;
        next.archived_at = archived.then_some(now);
        if archived {
            next.auto_resume_at = None;
            next.auto_resume_attempts = None;
        }
        self.replace_record(run_id, previous, next).map(Some)
    }

    pub fn archive(&mut self, run_id: &str, archived: bool) -> io::Result<Option<RunRecord>> {
        self.set_archived(run_id, archived)
    }

    /// Archive every terminal run in one durable write and return the number changed.
    pub fn archive_finished(&mut self) -> io::Result<usize> {
        let now = now_iso8601();
        let previous = self.runs.clone();
        let mut changed = Vec::new();
        for run in self.runs.values_mut() {
            if !run.archived
                && matches!(
                    run.status,
                    RunStatus::Done | RunStatus::Failed | RunStatus::Cancelled
                )
            {
                run.archived = true;
                run.archived_at = Some(now.clone());
                run.auto_resume_at = None;
                run.auto_resume_attempts = None;
                changed.push(run.clone());
            }
        }
        if changed.is_empty() {
            return Ok(0);
        }
        if let Err(error) = self.persist() {
            self.runs = previous;
            return Err(error);
        }
        for run in &changed {
            self.notify_run(run);
        }
        Ok(changed.len())
    }

    pub fn set_read(&mut self, run_id: &str) -> io::Result<Option<RunRecord>> {
        let Some(previous) = self.runs.get(run_id).cloned() else {
            return Ok(None);
        };
        let mut next = previous.clone();
        next.seen_at = Some(now_iso8601());
        self.replace_record(run_id, previous, next).map(Some)
    }

    pub fn mark_read(&mut self, run_id: &str) -> io::Result<Option<RunRecord>> {
        self.set_read(run_id)
    }

    pub fn set_unread(&mut self, run_id: &str) -> io::Result<Option<RunRecord>> {
        let Some(previous) = self.runs.get(run_id).cloned() else {
            return Ok(None);
        };
        let mut next = previous.clone();
        next.seen_at = None;
        self.replace_record(run_id, previous, next).map(Some)
    }

    pub fn mark_unread(&mut self, run_id: &str) -> io::Result<Option<RunRecord>> {
        self.set_unread(run_id)
    }

    /// Stamp currently unread finished runs and return the number stamped.
    pub fn mark_all_read(&mut self) -> io::Result<usize> {
        let now = now_iso8601();
        let previous = self.runs.clone();
        let mut changed = Vec::new();
        for run in self.runs.values_mut() {
            if is_unread(run) {
                run.seen_at = Some(now.clone());
                changed.push(run.clone());
            }
        }
        if changed.is_empty() {
            return Ok(0);
        }
        if let Err(error) = self.persist() {
            self.runs = previous;
            return Err(error);
        }
        for run in &changed {
            self.notify_run(run);
        }
        Ok(changed.len())
    }
}
