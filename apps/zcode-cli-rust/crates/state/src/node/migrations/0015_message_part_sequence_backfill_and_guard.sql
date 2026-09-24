
      with session_max as (
        select session_id, coalesce(max(sequence), -1) as max_sequence
        from message
        group by session_id
      ),
      ordered_null_message as (
        select
          m.id as id,
          sm.max_sequence + row_number() over (
            partition by m.session_id
            order by m.time_created, m.rowid
          ) as stable_sequence
        from message m
        join session_max sm on sm.session_id = m.session_id
        where m.sequence is null
      )
      update message
      set sequence = (
        select stable_sequence
        from ordered_null_message
        where ordered_null_message.id = message.id
      )
      where sequence is null;

      with message_max as (
        select message_id, coalesce(max(sequence), -1) as max_sequence
        from part
        group by message_id
      ),
      ordered_null_part as (
        select
          p.id as id,
          mm.max_sequence + row_number() over (
            partition by p.message_id
            order by p.time_created, p.rowid
          ) as stable_sequence
        from part p
        join message_max mm on mm.message_id = p.message_id
        where p.sequence is null
      )
      update part
      set sequence = (
        select stable_sequence
        from ordered_null_part
        where ordered_null_part.id = part.id
      )
      where sequence is null;

      create trigger if not exists message_sequence_autofill
      after insert on message
      when new.sequence is null
      begin
        update message
        set sequence = (
          select coalesce(max(sequence), -1) + 1
          from message
          where session_id = new.session_id
        )
        where id = new.id;
      end;

      create trigger if not exists part_sequence_autofill
      after insert on part
      when new.sequence is null
      begin
        update part
        set sequence = (
          select coalesce(max(sequence), -1) + 1
          from part
          where message_id = new.message_id
        )
        where id = new.id;
      end;
    