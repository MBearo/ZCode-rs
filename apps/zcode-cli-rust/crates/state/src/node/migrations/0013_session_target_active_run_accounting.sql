
      alter table session_target add column active_input_id text;
      alter table session_target add column active_run_started_at integer;
      alter table session_target add column active_run_last_seen_at integer;
    