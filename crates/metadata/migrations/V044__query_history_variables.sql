ALTER TABLE query_history
    ADD COLUMN variable_descriptors_json TEXT NOT NULL DEFAULT '[]';
