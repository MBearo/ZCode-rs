
      alter table session add column trace_id text;

      create index if not exists session_trace_idx on session(trace_id);
    