WITH schedules AS (
    SELECT r.id,'routine' AS kind,r.name,r.agent,r.project,r.next_at,r.interval_ms,r.revision,
           r.schedule_json AS spec_json,r.schedule_zone,json_extract(r.schedule_json,'$.kind') IS 'cron' AS cron,
           CASE WHEN r.enabled=1 THEN 'scheduled' ELSE 'paused' END AS status
    FROM routines r
    WHERE r.trigger='schedule' AND NOT EXISTS(SELECT 1 FROM local_routine_deletions d WHERE d.routine=r.id)
      AND (r.enabled=1 OR NOT EXISTS(SELECT 1 FROM routine_runs rr WHERE rr.routine=r.id AND rr.scheduled_at=r.next_at))
    UNION ALL
    SELECT w.id,'workflow',w.name,w.agent,b.project,s.next_at,s.interval_ms,s.revision,
           NULL,NULL,0,CASE WHEN s.enabled=1 THEN 'scheduled' ELSE 'paused' END
    FROM workflow_schedules s JOIN workflows w ON w.id=s.workflow
    LEFT JOIN local_workflow_schedule_projects b ON b.workflow=w.id
    WHERE w.deleted=0
      AND (s.enabled=1 OR NOT EXISTS(SELECT 1 FROM workflow_occurrences o WHERE o.workflow=w.id AND o.scheduled_at=s.next_at))
)
SELECT * FROM schedules s
WHERE (:project IS NULL OR coalesce(s.project,'none')=:project)
  AND (:agent IS NULL OR s.agent=:agent)
  AND (:kind IS NULL OR s.kind=:kind)
  AND (:status IS NULL OR s.status=:status)
  AND NOT EXISTS(SELECT 1 FROM agent_visibility v WHERE v.agent=s.agent AND v.hidden=1)
  AND s.next_at<:to
  AND (s.next_at>=:from OR (s.status='scheduled' AND (s.interval_ms IS NOT NULL OR s.cron)))
ORDER BY s.next_at,s.kind,s.id
LIMIT :limit
