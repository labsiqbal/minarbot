CREATE TABLE IF NOT EXISTS provider_instances(
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    display_name TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1 CHECK(enabled IN(0,1)),
    binary_path TEXT NOT NULL DEFAULT '',
    home_path TEXT NOT NULL DEFAULT '',
    shadow_home_path TEXT NOT NULL DEFAULT '',
    launch_args TEXT NOT NULL DEFAULT '[]' CHECK(json_valid(launch_args)),
    environment TEXT NOT NULL DEFAULT '[]' CHECK(json_valid(environment)),
    accent_color TEXT NOT NULL DEFAULT '',
    revision INTEGER NOT NULL DEFAULT 1 CHECK(revision>0),
    created TEXT NOT NULL DEFAULT '',
    updated TEXT NOT NULL DEFAULT ''
) STRICT;
