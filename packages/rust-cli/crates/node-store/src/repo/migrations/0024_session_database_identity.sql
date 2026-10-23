
    CREATE TABLE IF NOT EXISTS zcode_database_identity (
      scope TEXT PRIMARY KEY,
      identity TEXT NOT NULL
    );
    SELECT scope, identity FROM zcode_database_identity WHERE 0;
  