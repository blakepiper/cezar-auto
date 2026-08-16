import { z } from 'zod';
// `StartTodoResponse` embeds a whole run record, which belongs to the runs slice.
import { runRecordSchema } from './runs.ts';

// ---- skills (`GET /skills`) ----------------------------------------------------------------

/**
 * One discovered skill: repo (`.ai/skills`, `.ai/coducktor/skills`) or `npx skills` install dirs
 * (project + global). Remote team skills are retired (A15, decision 7 — spec §16a Tier 2):
 * local discovery is all that remains.
 */
export const skillSchema = z.object({
  name: z.string(),
  description: z.string().optional(),
  /** Advisory hint for untouched composer run-mode choices. */
  interactive: z.literal(true).optional(),
  body: z.string(),
  path: z.string(),
  source: z.enum(['ai', 'cezar', 'agents', 'global']),
});
export type Skill = z.infer<typeof skillSchema>;

// ---- follow-up inbox / todos (spec 007) ---------------------------------------------------

/** One entry of `.ai/coducktor/todos.json`, as `GET /todos` serves it (ids are backfilled on read). */
export const todoItemSchema = z.object({
  id: z.string(),
  ts: z.string().optional(),
  taskId: z.string().optional(),
  summary: z.string().min(1),
  action: z.string().optional(),
  prUrl: z.string().optional(),
  suggestedSkill: z.string().optional(),
  suggestedArgs: z.string().optional(),
  suggestedPrompt: z.string().optional(),
  /** Explicit intent; missing infers from suggestedSkill/suggestedPrompt for old files. */
  runnable: z.boolean().optional(),
  /** Set once a task was started from this entry — it then leaves the inbox and stays as
   *  the audit trail. A later launch never overwrites the first. */
  startedTaskId: z.string().optional(),
});
export type TodoItem = z.infer<typeof todoItemSchema>;

/**
 * `DELETE /todos/:id` — Dismiss checks the entry off.
 *
 * `removed` is the LITERAL `true`: a miss is a 404 `{ error }`, never `{ removed: false }`.
 * The hand-written DTO said `boolean`, which was wider than the route.
 */
export const removeTodoResponseSchema = z.object({
  removed: z.literal(true),
});
export type RemoveTodoResponse = z.infer<typeof removeTodoResponseSchema>;

/** `POST /todos/:id/start` — 201 with the run the entry became. */
export const startTodoResponseSchema = z.object({
  run: runRecordSchema,
});
export type StartTodoResponse = z.infer<typeof startTodoResponseSchema>;
