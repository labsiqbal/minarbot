UPDATE agents SET library_docs_access=1 WHERE library_docs_access=0;
UPDATE local_projects SET codegraph_enabled=1 WHERE codegraph_enabled=0;
DROP TABLE IF EXISTS craft_grants;

UPDATE agent_edits
SET request=json_remove(request,'$.libraryDocsAccess'),
    result=json_set(result,'$.library_docs_access',1)
WHERE json_type(request,'$.libraryDocsAccess') IS NOT NULL
   OR json_extract(result,'$.library_docs_access')=0;

UPDATE local_project_commands
SET request=json_remove(request,'$.codegraphEnabled'),
    result=json_set(result,'$.codegraphEnabled',json('true'))
WHERE json_type(request,'$.codegraphEnabled') IS NOT NULL
   OR json_extract(result,'$.codegraphEnabled')=0;
