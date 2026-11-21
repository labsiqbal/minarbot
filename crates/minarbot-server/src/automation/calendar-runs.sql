WITH runs AS (
    SELECT r.id,'routine' AS kind,coalesce(p.name,r.name) AS name,r.agent,pr.project,
           coalesce(rr.scheduled_at,unixepoch(t.created,'subsec')*1000) AS at,
           CASE WHEN t.paused=1 THEN 'paused'
                WHEN p.status IS NOT NULL THEN p.status
                WHEN t.status IN ('queued','running') OR EXISTS(
                    SELECT 1 FROM flow_steps f WHERE f.root_turn=t.id AND f.status IN ('waiting','queued')
                ) THEN 'running'
                WHEN EXISTS(SELECT 1 FROM flow_steps f WHERE f.root_turn=t.id AND f.status='failed') THEN 'failed'
                WHEN t.status='done' AND t.no_update=1 THEN 'no_update'
                WHEN t.status='done' THEN 'done' ELSE 'failed' END AS status,
           t.id AS run,t.thread,CASE WHEN p.posted=1 THEN p.destination END AS report_to,p.message AS report,
           NOT EXISTS(SELECT 1 FROM local_routine_deletions d WHERE d.routine=r.id) AS editable
    FROM routine_runs rr JOIN routines r ON r.id=rr.routine JOIN turns t ON t.id=rr.turn
    JOIN threads h ON h.id=t.thread AND h.deleted=0
    LEFT JOIN local_project_threads pr ON pr.thread=t.thread
    LEFT JOIN local_routine_reports p ON p.run=t.id
    UNION ALL
    SELECT w.id,'workflow',w.name,w.agent,p.project,coalesce(o.scheduled_at,unixepoch(r.created,'subsec')*1000),
           CASE WHEN r.status IN ('ready','waiting','approval') THEN 'running'
                WHEN r.status='done' THEN 'done' ELSE 'failed' END,r.id,NULL,NULL,NULL,w.deleted=0
    FROM workflow_runs r JOIN workflows w ON w.id=r.workflow
    LEFT JOIN workflow_occurrences o ON o.run=r.id
    LEFT JOIN local_workflow_run_projects p ON p.run=r.id
    UNION ALL
    SELECT w.id,'workflow',w.name,w.agent,p.project,o.scheduled_at,'no_update',NULL,NULL,NULL,NULL,w.deleted=0
    FROM workflow_occurrences o JOIN workflows w ON w.id=o.workflow
    LEFT JOIN local_workflow_schedule_projects p ON p.workflow=w.id
    WHERE o.run IS NULL
    UNION ALL
    SELECT r.id,'routine',r.name,r.agent,r.project,o.scheduled_at,'no_update',NULL,NULL,NULL,NULL,
           NOT EXISTS(SELECT 1 FROM local_routine_deletions d WHERE d.routine=r.id)
    FROM agent_visibility_skips o JOIN routines r ON r.id=o.routine
)
SELECT id,kind,name,agent,project,CAST(round(at) AS INTEGER) AS at,status,run,thread,report_to,report,editable
FROM runs
WHERE (:project IS NULL OR coalesce(project,'none')=:project) AND (:agent IS NULL OR agent=:agent)
  AND (:kind IS NULL OR kind=:kind) AND (:status IS NULL OR status=:status)
  AND at>=:from AND at<:to
ORDER BY status!='running',at DESC,kind,id,run
LIMIT :limit
