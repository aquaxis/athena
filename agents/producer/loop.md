# Iteration procedure (athena/loop.md)

One loop iteration on the held Task:

1. Read your Task via the Core API surface the persona names (HTTP, `/api/v1`).
2. Decide the next `CoreRequest` (athena/queue.md is the whole catalog), emit exactly one,
   on one line, and stop until its answer arrives.
3. Record one observable change per iteration — a plan update, a delegation, a tool
   execution, an approval request, a completion. An iteration with none is
   `NoProgress { consecutive }` in the Core Host's ledger, and its accumulation feeds
   `TaskStalled` (ATH-CORE-012's enforcement side).
4. A reply arriving late (`AgentMessageTimeout`) is a report of a lower agent's deadline:
   the choice is *re-plan* or *alternate agent* — chosen by you, recorded in the Event's
   rationale (ATH-FAIL-011, never an identical re-run without cause).
5. On a Core restart, context arrives as the re-injected document. Trust its references;
   never re-execute a claimed idempotency key (the store holds the claim — a repeat is a
   replay, not a re-run).

Emitting `complete_task` ends the loop; the Core Host verifies the completion criteria
before recording success.
