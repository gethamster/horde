pub const OPERATIONS: &[(&str, &str)] = &[
    (
        "skill_pack_list",
        "Inspect this runtime's file-based default skill pack and content hash",
    ),
    (
        "skill_pack_install",
        "Install a local directory of skills for new tasks, independently of the runtime binary",
    ),
    (
        "runtime_skills_update",
        "Send this controller's current default skill pack to a worker by name or ID; request_id makes retries safe and running tasks retain their pins",
    ),
    (
        "runtime_capabilities",
        "Discover local and connected worker executors, models, capacity, and authentication evidence before choosing delegation",
    ),
    (
        "plan_execution",
        "Resolve work roles into allowed machine/model pools; reports blockers and leaves task choices to the parent agent",
    ),
    (
        "agent_setup",
        "Inspect or arrange Horde setup using available access; report missing credentials or access without exposing secrets",
    ),
    (
        "skill_inspect",
        "Read shipped skills and effective project instructions, including content and version hashes",
    ),
    (
        "skill_propose",
        "Draft a project skill change for discussion; does not activate it",
    ),
    (
        "skill_apply",
        "Apply an explicitly accepted project skill proposal; running tasks keep their pinned versions",
    ),
    (
        "skill_history",
        "Inspect the project's saved skill revisions",
    ),
    (
        "skill_rollback",
        "Propose restoring a previous project skill revision for acceptance",
    ),
    (
        "runtime_updates_resume",
        "Resume queued fleet updates after inspecting a failed rollout",
    ),
    (
        "runtime_reconcile",
        "Inspect and adopt a provider resource after uncertain provisioning",
    ),
    (
        "runtime_list",
        "List workers and managed runtimes by name and status",
    ),
    ("runtime_rename", "Assign a memorable name to a runtime"),
    (
        "runtime_forget",
        "Remove a disconnected runtime entry without deleting its host",
    ),
    ("runtime_inspect", "Inspect managed runtime"),
    ("runtime_create", "Create managed runtime"),
    ("runtime_destroy", "Destroy managed runtime"),
    ("runtime_restart", "Restart managed runtime"),
    (
        "runtime_update",
        "Request a signed runtime update on a connected worker by name or ID; inspect the operation until completion",
    ),
    ("runtime_stop", "Stop managed runtime"),
    ("runtime_start", "Start managed runtime"),
    ("runtime_config_get", "Read runtime concurrency"),
    (
        "runtime_config_set",
        "Change runtime concurrency without interrupting work",
    ),
    ("runtime_drain", "Stop new dispatches"),
    ("runtime_resume", "Resume runtime dispatch"),
    ("runtime_status", "Inspect version and drain status"),
    ("account_status", "Inspect account capacity and freshness"),
    (
        "account_observe",
        "Record an explicit account quota observation",
    ),
    ("management_events", "Read runtime management events"),
    ("management_ack", "Acknowledge management events"),
    (
        "integrate_child",
        "Import a completed child and verify the combined result",
    ),
    (
        "environments",
        "Inspect owned application environments and cleanup state",
    ),
    (
        "refresh_bundles",
        "Explicitly refresh selected application bundle versions",
    ),
    (
        "delegate_task",
        "Create a bounded child task; id is required for retry deduplication",
    ),
    ("list_children", "Inspect immediate child tasks"),
    (
        "list_skills",
        "List the task’s pinned skill names and hashes",
    ),
    (
        "read_skill",
        "Read a pinned skill or relative resource; default path SKILL.md, paged by byte offset",
    ),
    (
        "read_context",
        "Read original source context using bounded pages",
    ),
    (
        "update_context",
        "Root caller: append authoritative context with provenance and invalidate stale acceptance",
    ),
    (
        "pending_questions",
        "Read questions addressed to this caller",
    ),
    (
        "escalate_question",
        "Forward the original question one level to your parent",
    ),
    (
        "ack_events",
        "Acknowledge durable events through a consumer cursor",
    ),
    (
        "propose_steps",
        "Planner-only: propose parallel or dependent steps. Runtime validates and inserts them before the planning step’s pending successors",
    ),
    (
        "request_question",
        "Ask for required information; hold this worker while its caller answers or escalates",
    ),
    (
        "metrics",
        "Reported usage, cost, latency, retries, and coordination overhead",
    ),
    (
        "submit_task",
        "Submit work with a stable request_id for safe retries, optional runtime and execution pool; returns durable task id",
    ),
    (
        "remote_result",
        "Get a local checkout of completed remote work for review",
    ),
    (
        "inspect",
        "Inspect a task including steps, attempts, workers, questions, and integration",
    ),
    (
        "summary",
        "Terminal summary: status, step outcomes, integrated head, delivery outcome",
    ),
    ("list_tasks", "List submitted tasks and their status"),
    ("events", "Read ordered activity events"),
    ("cancel", "Cancel a task"),
    (
        "resume",
        "Resume paused or failed work; interrupted processes require reconciliation",
    ),
    ("answer_question", "Answer a pending question"),
    (
        "register_worker",
        "Create task-scoped worker identity and token",
    ),
    (
        "register_workspace",
        "Register path, branch and base before editing",
    ),
    (
        "send_message",
        "Persist a message to worker id, group:name, or task. Supply id for deduplication",
    ),
    (
        "steer",
        "Operator: post a message to workers on a task; omit worker to fan out, or set worker to target one",
    ),
    (
        "read_messages",
        "Read unacknowledged messages; after is an optional sequence cursor",
    ),
    (
        "acknowledge_messages",
        "Acknowledge an explicit list of message ids",
    ),
    ("list_workers", "List worker identities and status"),
    (
        "set_worker_status",
        "Set idle, working, blocked, or stopped status",
    ),
    ("join_channel", "Join a named group channel"),
    (
        "claim_paths",
        "Acquire exclusive paths in a registered workspace",
    ),
    (
        "transfer_claim",
        "Atomically hand off a claim to another worker",
    ),
    (
        "release_claims",
        "Release claims after worker is stopped or idle",
    ),
    (
        "reconcile_worker",
        "Confirm an interrupted process exited, then make step resumable",
    ),
    (
        "add_steps",
        "Append validated steps as a durable workflow revision",
    ),
    (
        "put_artifact",
        "Store content with input fingerprint and verification status",
    ),
    ("get_artifact", "Read a task artifact by hash"),
    (
        "reuse_artifact",
        "Find a verified artifact with identical input fingerprint",
    ),
    (
        "add_knowledge",
        "Store fact, decision or evidence with provenance",
    ),
    ("knowledge", "Read knowledge for this task"),
    (
        "link_knowledge",
        "Link two knowledge records with a relationship",
    ),
    (
        "integrate",
        "Integrate a committed worker worktree; optional validation argv",
    ),
];
