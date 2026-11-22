CREATE TABLE IF NOT EXISTS library_docs_cache(
    library TEXT NOT NULL, version TEXT NOT NULL, topic TEXT NOT NULL,
    result TEXT NOT NULL CHECK(json_valid(result)), expires INTEGER NOT NULL,
    PRIMARY KEY(library,version,topic)
) STRICT;
