
      create table if not exists local_setting (
        scope text not null,
        scope_id text not null,
        namespace text not null,
        key text not null,
        value text not null,
        schema_version integer not null,
        time_created integer not null,
        time_updated integer not null,
        primary key(scope, scope_id, namespace, key)
      );

      create index if not exists local_setting_scope_idx
        on local_setting(scope, scope_id);

      create index if not exists local_setting_namespace_key_idx
        on local_setting(namespace, key);
    