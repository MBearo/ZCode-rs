
      alter table workflow_definition
        add column scope text not null default 'explicit'
        check(scope in ('builtin', 'explicit', 'project', 'user'));
    