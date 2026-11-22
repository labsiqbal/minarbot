-- Retired goals and a disabled loop keep background schedulers idle.
INSERT INTO agents(id,name,model,instructions) VALUES('rollback-agent','Rollback','fixture','Fixture');
INSERT INTO turns(id,agent,text,status,reply,created)
VALUES('rollback-turn','rollback-agent','Preserve rollback data','done','Saved reply','2026-01-01T00:00:00Z');
INSERT INTO goals(id,scope,target,request,max_turns,max_minutes,max_tokens,deadline,current_turn,status)
VALUES('rollback-turn','fixture','Check','{}',3,5,1000,0,'rollback-turn','met');
INSERT INTO goal_turns(turn,goal,number) VALUES('rollback-turn','rollback-turn',1);
INSERT INTO goal_verdicts VALUES('rollback-turn',1,'Complete','[]','fixture');
INSERT INTO goal_judge_models VALUES('rollback-agent','api','fixture');
INSERT INTO routines(id,agent,name,prompt,next_at,enabled)
VALUES('rollback-loop','rollback-agent','Loop','Check',0,0);
INSERT INTO routine_loops(routine,request,until_condition,max_runs)
VALUES('rollback-loop','{}','Ready',3);

-- History mirrors populate memory_units and the FTS5 index through real triggers.
INSERT INTO memories(scope,id,text,essential,revision,removed)
VALUES('agents:rollback-agent','note','Recallable snapshot',0,1,0);
INSERT INTO memory_learning_turns VALUES('rollback-turn','rollback-agent');
INSERT INTO memory_extraction(raw,done)
SELECT id,1 FROM memory_units WHERE origin='prompt' AND source_id='rollback-turn';
INSERT INTO memory_units(bank,origin,source_id,kind,text,source) VALUES
  ('agents:rollback-agent','retained','fact','world','Saved fact','{}'),
  ('agents:rollback-agent','retained','observation','observation','Saved observation','{}');
INSERT INTO memory_facts(unit,raw,offset,consolidated)
SELECT fact.id,raw.id,0,1 FROM memory_units fact,memory_units raw
WHERE fact.source_id='fact' AND raw.origin='prompt' AND raw.source_id='rollback-turn';
INSERT INTO memory_observations(unit,bank,agent,normalized)
SELECT id,bank,'rollback-agent','saved observation' FROM memory_units WHERE source_id='observation';
INSERT INTO memory_observation_evidence
SELECT observation.unit,fact.unit FROM memory_observations observation,memory_facts fact;
INSERT INTO memory_unit_entities
SELECT bank,id,'snapshot' FROM memory_units WHERE source_id='note';
INSERT INTO memory_links
SELECT fact.bank,fact.id,observation.id,'entity' FROM memory_units fact,memory_units observation
WHERE fact.source_id='fact' AND observation.source_id='observation';

INSERT INTO html_replies VALUES('html','rollback-turn','html-call','Reply',100,'<p>Saved</p>');
INSERT INTO html_reply_context VALUES('rollback-turn','{}');
INSERT INTO component_replies VALUES('component','rollback-turn','component-call','{}',NULL);
INSERT INTO mcp_connections VALUES('connection','Fixture','http://127.0.0.1:1','','[]',0,1);
INSERT INTO mcp_apps VALUES('app','rollback-turn','connection',1,'Saved app','<p>Saved</p>','{}');
INSERT INTO mcp_app_approvals VALUES('app','approval','fixture',0);

INSERT INTO local_projects(id,name,description,created,updated,channel)
VALUES('project','Workspace','',0,0,'');
INSERT INTO craft_grants VALUES('rollback-agent');
INSERT INTO craft_deep VALUES('project');
