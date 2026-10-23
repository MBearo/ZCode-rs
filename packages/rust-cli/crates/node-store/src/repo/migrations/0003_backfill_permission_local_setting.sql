
      insert or ignore into local_setting (
        scope,
        scope_id,
        namespace,
        key,
        value,
        schema_version,
        time_created,
        time_updated
      )
      select
        'project',
        project_id,
        'permission',
        'ruleset',
        data,
        1,
        time_created,
        time_updated
      from permission
      where data is not null;
    