-- Captured from fresh device-host startup at 85373b3 before collapse.
PRAGMA application_id=1296978481;
PRAGMA user_version=3;
CREATE TABLE device_enrollment_keys(authority_generation TEXT NOT NULL,device_generation INTEGER NOT NULL,signing_ref TEXT NOT NULL,signing_public_key TEXT NOT NULL,PRIMARY KEY(authority_generation,device_generation)) STRICT;
CREATE TABLE device_journal(action_id TEXT PRIMARY KEY, task_id TEXT NOT NULL, sequence INTEGER NOT NULL, action_kind TEXT NOT NULL CHECK(action_kind IN('provider','canonical','resource','read','compute','local','browser')), authority_generation TEXT NOT NULL, device_generation INTEGER NOT NULL, executor_generation INTEGER NOT NULL, state TEXT NOT NULL CHECK(state IN('prepared','committed','in_flight','reported','acknowledged','not_sent','uncertain')), payload_digest TEXT NOT NULL CHECK(length(payload_digest)=64), intent_digest TEXT NOT NULL CHECK(length(intent_digest)=64), intent_json TEXT NOT NULL CHECK(json_valid(intent_json)), binding_id TEXT, binding_revision INTEGER, grant_revision INTEGER NOT NULL, result_digest TEXT, result_json TEXT CHECK(result_json IS NULL OR json_valid(result_json)), effect_status TEXT NOT NULL CHECK(effect_status IN('none','known','unknown')), uncertain_reason TEXT, send_marker INTEGER NOT NULL CHECK(send_marker IN(0,1)), journal_sequence INTEGER NOT NULL, entry_signature TEXT, created INTEGER NOT NULL, CHECK((action_kind='provider' AND binding_id IS NOT NULL AND binding_revision IS NOT NULL) OR (action_kind<>'provider' AND binding_id IS NULL AND binding_revision IS NULL)), UNIQUE(task_id,sequence)) STRICT;
CREATE TABLE device_outbox(action_id TEXT PRIMARY KEY REFERENCES device_journal(action_id), report_digest TEXT NOT NULL CHECK(length(report_digest)=64), state TEXT NOT NULL CHECK(state IN('pending','acknowledged')), receipt_json TEXT CHECK(receipt_json IS NULL OR json_valid(receipt_json)), report_json TEXT CHECK(report_json IS NULL OR json_valid(report_json))) STRICT;
CREATE TABLE device_provider_bindings(binding_id TEXT PRIMARY KEY, provider_id TEXT NOT NULL CHECK(provider_id IN('openai-compatible','anthropic','gemini')), provider_account_id TEXT NOT NULL, revision INTEGER NOT NULL CHECK(revision>0), status TEXT NOT NULL CHECK(status IN('missing','ready','unreadable','unavailable','revoked')), key_reference TEXT, configuration_json TEXT NOT NULL CHECK(json_valid(configuration_json)), UNIQUE(provider_id,provider_account_id)) STRICT;
CREATE TABLE device_recovery_entries(action_id TEXT NOT NULL REFERENCES device_journal(action_id),journal_sequence INTEGER NOT NULL,entry_json TEXT NOT NULL CHECK(json_valid(entry_json)),PRIMARY KEY(action_id,journal_sequence)) STRICT;
CREATE TABLE device_resource_bindings(binding_id TEXT PRIMARY KEY, project_id TEXT NOT NULL, directory TEXT NOT NULL, write INTEGER NOT NULL CHECK(write IN(0,1)), revision INTEGER NOT NULL CHECK(revision>0), deleted INTEGER NOT NULL CHECK(deleted IN(0,1))) STRICT;
CREATE TABLE device_state(id INTEGER PRIMARY KEY CHECK(id=1), device_id TEXT NOT NULL, authority_generation TEXT NOT NULL, device_generation INTEGER NOT NULL, highest_executor_generation INTEGER NOT NULL, journal_sequence INTEGER NOT NULL, executor_refs_json TEXT NOT NULL CHECK(json_valid(executor_refs_json))) STRICT;
CREATE TABLE device_wait_secrets(
        wait_key TEXT NOT NULL,
        kind TEXT NOT NULL CHECK(kind IN('turn','workflow')),
        key_reference TEXT NOT NULL UNIQUE,
        secret_generation INTEGER NOT NULL CHECK(secret_generation>0),
        deadline INTEGER NOT NULL,
        closed INTEGER NOT NULL CHECK(closed IN(0,1)),
        provision_id TEXT NOT NULL UNIQUE,
        payload_digest TEXT NOT NULL CHECK(length(payload_digest)=64),
        provision_json TEXT NOT NULL CHECK(json_valid(provision_json)),
        result_request_id TEXT NOT NULL UNIQUE,
        result_json TEXT CHECK(result_json IS NULL OR json_valid(result_json)),
        state TEXT NOT NULL CHECK(state IN('staging','ready','failed','acknowledged')),
        ack_json TEXT CHECK(ack_json IS NULL OR json_valid(ack_json)),
        cleanup_pending INTEGER NOT NULL DEFAULT 0 CHECK(cleanup_pending IN(0,1)),
        PRIMARY KEY(wait_key,secret_generation),
        CHECK((state='staging' AND result_json IS NULL) OR (state<>'staging' AND result_json IS NOT NULL))
      ) STRICT;
CREATE INDEX device_wait_cleanup ON device_wait_secrets(cleanup_pending) WHERE closed=1;
CREATE INDEX device_wait_recovery ON device_wait_secrets(state,json_extract(provision_json,'$.sourceDeviceGeneration'),json_extract(provision_json,'$.authorityGeneration'));
