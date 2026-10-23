
  -- 无 entry 时沿用既有最后一个明确消息来源规则；不能越过损坏/明确空选择找更早的值。
  with ranked as (
    select message.*, row_number() over (
      partition by session_id order by sequence desc, time_created desc, rowid desc
    ) as rank
    from message
    where json_valid(data) and json_type(data) = 'object'
      and ((json_extract(data, '$.role') = 'user' and (json_type(data, '$.model') is not null or json_type(data, '$.modelSelection') is not null))
        or (json_extract(data, '$.role') = 'assistant' and (json_type(data, '$.providerID') is not null or json_type(data, '$.modelID') is not null
          or json_type(data, '$.providerId') is not null or json_type(data, '$.modelId') is not null or json_type(data, '$.reasoningLevel') is not null)))
      and not exists (select 1 from session_entry e where e.session_id = message.session_id and e.type = 'runtime/model_selection')
  ), candidates as (
    select *, case
  when json_extract(data, '$.role') = 'user' then
    case when json_type(data, '$.modelSelection') is not null
      then case when substr(json_extract(data, '$.modelSelection.providerId'), 1, 8) = 'builtin:'
        then case when (typeof(json_extract(data, '$.modelSelection.providerId')) = 'text' and length(trim(json_extract(data, '$.modelSelection.providerId'))) > 0) and (typeof(json_extract(data, '$.modelSelection.modelId')) = 'text' and length(trim(json_extract(data, '$.modelSelection.modelId'))) > 0) then
    json_patch(
      json_patch(json_object('providerId', json_extract(data, '$.modelSelection.providerId'), 'modelId', json_extract(data, '$.modelSelection.modelId')),
        case when (typeof(json_extract(data, '$.modelSelection.options.reasoningLevel')) = 'text' and length(trim(json_extract(data, '$.modelSelection.options.reasoningLevel'))) > 0) then json_object('options', json_object('reasoningLevel', json_extract(data, '$.modelSelection.options.reasoningLevel'))) else '{}' end),
      case when typeof(NULL) = 'text' then json_object('label', NULL) else '{}' end)
    else NULL end
        else NULL end
      else case when (typeof(json_extract(data, '$.model.providerID')) = 'text' and length(trim(json_extract(data, '$.model.providerID'))) > 0) and (typeof(json_extract(data, '$.model.modelID')) = 'text' and length(trim(json_extract(data, '$.model.modelID'))) > 0) then
    json_patch(
      json_patch(json_object('providerId', json_extract(data, '$.model.providerID'), 'modelId', json_extract(data, '$.model.modelID')),
        case when (typeof(json_extract(data, '$.model.variant')) = 'text' and length(trim(json_extract(data, '$.model.variant'))) > 0) then json_object('options', json_object('reasoningLevel', json_extract(data, '$.model.variant'))) else '{}' end),
      case when typeof(NULL) = 'text' then json_object('label', NULL) else '{}' end)
    else NULL end end
  else case when json_type(data, '$.providerId') is not null or json_type(data, '$.modelId') is not null or json_type(data, '$.reasoningLevel') is not null
    then case when substr(json_extract(data, '$.providerId'), 1, 8) = 'builtin:'
      then case when (typeof(json_extract(data, '$.providerId')) = 'text' and length(trim(json_extract(data, '$.providerId'))) > 0) and (typeof(json_extract(data, '$.modelId')) = 'text' and length(trim(json_extract(data, '$.modelId'))) > 0) then
    json_patch(
      json_patch(json_object('providerId', json_extract(data, '$.providerId'), 'modelId', json_extract(data, '$.modelId')),
        case when (typeof(json_extract(data, '$.reasoningLevel')) = 'text' and length(trim(json_extract(data, '$.reasoningLevel'))) > 0) then json_object('options', json_object('reasoningLevel', json_extract(data, '$.reasoningLevel'))) else '{}' end),
      case when typeof(NULL) = 'text' then json_object('label', NULL) else '{}' end)
    else NULL end else NULL end
    else case when (typeof(json_extract(data, '$.providerID')) = 'text' and length(trim(json_extract(data, '$.providerID'))) > 0) and (typeof(json_extract(data, '$.modelID')) = 'text' and length(trim(json_extract(data, '$.modelID'))) > 0) then
    json_patch(
      json_patch(json_object('providerId', json_extract(data, '$.providerID'), 'modelId', json_extract(data, '$.modelID')),
        case when (typeof(json_extract(data, '$.variant')) = 'text' and length(trim(json_extract(data, '$.variant'))) > 0) then json_object('options', json_object('reasoningLevel', json_extract(data, '$.variant'))) else '{}' end),
      case when typeof(NULL) = 'text' then json_object('label', NULL) else '{}' end)
    else NULL end end end as candidate from ranked where rank = 1
  ), migrated as (
    select *, case when (typeof(case trim(json_extract(candidate, '$.providerId'))
    when 'builtin:bigmodel' then 'bigmodel-api'
    when 'builtin:zai' then 'zai-api'
    when 'builtin:bigmodel-start-plan' then 'account:bigmodel-start-plan'
    when 'builtin:zai-start-plan' then 'account:zai-start-plan'
    when 'builtin:bigmodel-coding-plan' then 'account:bigmodel-individual-coding-plan'
    when 'builtin:zai-coding-plan' then 'account:zai-individual-coding-plan'
    else case when substr(trim(json_extract(candidate, '$.providerId')), 1, 8) = 'builtin:' then NULL else trim(json_extract(candidate, '$.providerId')) end end) = 'text' and length(trim(case trim(json_extract(candidate, '$.providerId'))
    when 'builtin:bigmodel' then 'bigmodel-api'
    when 'builtin:zai' then 'zai-api'
    when 'builtin:bigmodel-start-plan' then 'account:bigmodel-start-plan'
    when 'builtin:zai-start-plan' then 'account:zai-start-plan'
    when 'builtin:bigmodel-coding-plan' then 'account:bigmodel-individual-coding-plan'
    when 'builtin:zai-coding-plan' then 'account:zai-individual-coding-plan'
    else case when substr(trim(json_extract(candidate, '$.providerId')), 1, 8) = 'builtin:' then NULL else trim(json_extract(candidate, '$.providerId')) end end)) > 0) and (typeof(trim(json_extract(candidate, '$.modelId'))) = 'text' and length(trim(trim(json_extract(candidate, '$.modelId')))) > 0) then
    json_patch(
      json_patch(json_object('providerId', case trim(json_extract(candidate, '$.providerId'))
    when 'builtin:bigmodel' then 'bigmodel-api'
    when 'builtin:zai' then 'zai-api'
    when 'builtin:bigmodel-start-plan' then 'account:bigmodel-start-plan'
    when 'builtin:zai-start-plan' then 'account:zai-start-plan'
    when 'builtin:bigmodel-coding-plan' then 'account:bigmodel-individual-coding-plan'
    when 'builtin:zai-coding-plan' then 'account:zai-individual-coding-plan'
    else case when substr(trim(json_extract(candidate, '$.providerId')), 1, 8) = 'builtin:' then NULL else trim(json_extract(candidate, '$.providerId')) end end, 'modelId', trim(json_extract(candidate, '$.modelId'))),
        case when (typeof(trim(json_extract(candidate, '$.options.reasoningLevel'))) = 'text' and length(trim(trim(json_extract(candidate, '$.options.reasoningLevel')))) > 0) then json_object('options', json_object('reasoningLevel', trim(json_extract(candidate, '$.options.reasoningLevel')))) else '{}' end),
      case when typeof(NULL) = 'text' then json_object('label', NULL) else '{}' end)
    else NULL end as normalized from candidates
  )
  insert into session_entry(id, session_id, type, time_created, time_updated, data)
    select session_id || ':runtime-model-selection', session_id, 'runtime/model_selection', time_created, time_updated,
      json_object('modelSelection', json(normalized))
    from migrated where normalized is not null
    on conflict(id) do nothing;

  update session_entry set data = json_set(data, '$.modelSelection', json(case when (typeof(case trim(json_extract(data, '$.providerId'))
    when 'builtin:bigmodel' then 'bigmodel-api'
    when 'builtin:zai' then 'zai-api'
    when 'builtin:bigmodel-start-plan' then 'account:bigmodel-start-plan'
    when 'builtin:zai-start-plan' then 'account:zai-start-plan'
    when 'builtin:bigmodel-coding-plan' then 'account:bigmodel-individual-coding-plan'
    when 'builtin:zai-coding-plan' then 'account:zai-individual-coding-plan'
    else case when substr(trim(json_extract(data, '$.providerId')), 1, 8) = 'builtin:' then NULL else trim(json_extract(data, '$.providerId')) end end) = 'text' and length(trim(case trim(json_extract(data, '$.providerId'))
    when 'builtin:bigmodel' then 'bigmodel-api'
    when 'builtin:zai' then 'zai-api'
    when 'builtin:bigmodel-start-plan' then 'account:bigmodel-start-plan'
    when 'builtin:zai-start-plan' then 'account:zai-start-plan'
    when 'builtin:bigmodel-coding-plan' then 'account:bigmodel-individual-coding-plan'
    when 'builtin:zai-coding-plan' then 'account:zai-individual-coding-plan'
    else case when substr(trim(json_extract(data, '$.providerId')), 1, 8) = 'builtin:' then NULL else trim(json_extract(data, '$.providerId')) end end)) > 0) and (typeof(trim(json_extract(data, '$.modelId'))) = 'text' and length(trim(trim(json_extract(data, '$.modelId')))) > 0) then
    json_patch(
      json_patch(json_object('providerId', case trim(json_extract(data, '$.providerId'))
    when 'builtin:bigmodel' then 'bigmodel-api'
    when 'builtin:zai' then 'zai-api'
    when 'builtin:bigmodel-start-plan' then 'account:bigmodel-start-plan'
    when 'builtin:zai-start-plan' then 'account:zai-start-plan'
    when 'builtin:bigmodel-coding-plan' then 'account:bigmodel-individual-coding-plan'
    when 'builtin:zai-coding-plan' then 'account:zai-individual-coding-plan'
    else case when substr(trim(json_extract(data, '$.providerId')), 1, 8) = 'builtin:' then NULL else trim(json_extract(data, '$.providerId')) end end, 'modelId', trim(json_extract(data, '$.modelId'))),
        case when (typeof(trim(json_extract(data, '$.thoughtLevel'))) = 'text' and length(trim(trim(json_extract(data, '$.thoughtLevel')))) > 0) then json_object('options', json_object('reasoningLevel', trim(json_extract(data, '$.thoughtLevel')))) else '{}' end),
      case when typeof(NULL) = 'text' then json_object('label', NULL) else '{}' end)
    else NULL end))
    where type = 'runtime/model_selection' and json_valid(data) and json_type(data) = 'object'
      and (typeof(json_extract(data, '$.providerId')) = 'text' and length(trim(json_extract(data, '$.providerId'))) > 0) and (typeof(json_extract(data, '$.modelId')) = 'text' and length(trim(json_extract(data, '$.modelId'))) > 0) and case when (typeof(case trim(json_extract(data, '$.providerId'))
    when 'builtin:bigmodel' then 'bigmodel-api'
    when 'builtin:zai' then 'zai-api'
    when 'builtin:bigmodel-start-plan' then 'account:bigmodel-start-plan'
    when 'builtin:zai-start-plan' then 'account:zai-start-plan'
    when 'builtin:bigmodel-coding-plan' then 'account:bigmodel-individual-coding-plan'
    when 'builtin:zai-coding-plan' then 'account:zai-individual-coding-plan'
    else case when substr(trim(json_extract(data, '$.providerId')), 1, 8) = 'builtin:' then NULL else trim(json_extract(data, '$.providerId')) end end) = 'text' and length(trim(case trim(json_extract(data, '$.providerId'))
    when 'builtin:bigmodel' then 'bigmodel-api'
    when 'builtin:zai' then 'zai-api'
    when 'builtin:bigmodel-start-plan' then 'account:bigmodel-start-plan'
    when 'builtin:zai-start-plan' then 'account:zai-start-plan'
    when 'builtin:bigmodel-coding-plan' then 'account:bigmodel-individual-coding-plan'
    when 'builtin:zai-coding-plan' then 'account:zai-individual-coding-plan'
    else case when substr(trim(json_extract(data, '$.providerId')), 1, 8) = 'builtin:' then NULL else trim(json_extract(data, '$.providerId')) end end)) > 0) and (typeof(trim(json_extract(data, '$.modelId'))) = 'text' and length(trim(trim(json_extract(data, '$.modelId')))) > 0) then
    json_patch(
      json_patch(json_object('providerId', case trim(json_extract(data, '$.providerId'))
    when 'builtin:bigmodel' then 'bigmodel-api'
    when 'builtin:zai' then 'zai-api'
    when 'builtin:bigmodel-start-plan' then 'account:bigmodel-start-plan'
    when 'builtin:zai-start-plan' then 'account:zai-start-plan'
    when 'builtin:bigmodel-coding-plan' then 'account:bigmodel-individual-coding-plan'
    when 'builtin:zai-coding-plan' then 'account:zai-individual-coding-plan'
    else case when substr(trim(json_extract(data, '$.providerId')), 1, 8) = 'builtin:' then NULL else trim(json_extract(data, '$.providerId')) end end, 'modelId', trim(json_extract(data, '$.modelId'))),
        case when (typeof(trim(json_extract(data, '$.thoughtLevel'))) = 'text' and length(trim(trim(json_extract(data, '$.thoughtLevel')))) > 0) then json_object('options', json_object('reasoningLevel', trim(json_extract(data, '$.thoughtLevel')))) else '{}' end),
      case when typeof(NULL) = 'text' then json_object('label', NULL) else '{}' end)
    else NULL end is not null;

  -- 已发布 User 消息也可能含 modelSelection；它本身不是可覆盖的未发布新目标字段。
  update message set data = json_set(data, '$.modelSelection', json(case when (typeof(json_extract(data, '$.model.providerID')) = 'text' and length(trim(json_extract(data, '$.model.providerID'))) > 0) and (typeof(json_extract(data, '$.model.modelID')) = 'text' and length(trim(json_extract(data, '$.model.modelID'))) > 0) then
    json_patch(
      json_patch(json_object('providerId', json_extract(data, '$.model.providerID'), 'modelId', json_extract(data, '$.model.modelID')),
        case when (typeof(json_extract(data, '$.model.variant')) = 'text' and length(trim(json_extract(data, '$.model.variant'))) > 0) then json_object('options', json_object('reasoningLevel', json_extract(data, '$.model.variant'))) else '{}' end),
      case when typeof(NULL) = 'text' then json_object('label', NULL) else '{}' end)
    else NULL end))
    where json_valid(data) and json_type(data) = 'object' and json_extract(data, '$.role') = 'user'
      and json_type(data, '$.modelSelection') is null and json_type(data, '$.model') = 'object';

  update message set data = json_patch(data, json_patch(
      json_object('providerId', json_extract(data, '$.providerID'), 'modelId', json_extract(data, '$.modelID')),
      case when (typeof(json_extract(data, '$.variant')) = 'text' and length(trim(json_extract(data, '$.variant'))) > 0) then json_object('reasoningLevel', json_extract(data, '$.variant')) else '{}' end))
    where json_valid(data) and json_type(data) = 'object' and json_extract(data, '$.role') = 'assistant'
      and (typeof(json_extract(data, '$.providerID')) = 'text' and length(trim(json_extract(data, '$.providerID'))) > 0) and (typeof(json_extract(data, '$.modelID')) = 'text' and length(trim(json_extract(data, '$.modelID'))) > 0);

  update part set data = json_set(data, '$.fromModelSelection', json(case when (typeof(json_extract(data, '$.fromModel.providerID')) = 'text' and length(trim(json_extract(data, '$.fromModel.providerID'))) > 0) and (typeof(json_extract(data, '$.fromModel.modelID')) = 'text' and length(trim(json_extract(data, '$.fromModel.modelID'))) > 0) then
    json_patch(
      json_patch(json_object('providerId', json_extract(data, '$.fromModel.providerID'), 'modelId', json_extract(data, '$.fromModel.modelID')),
        case when (typeof(json_extract(data, '$.fromModel.variant')) = 'text' and length(trim(json_extract(data, '$.fromModel.variant'))) > 0) then json_object('options', json_object('reasoningLevel', json_extract(data, '$.fromModel.variant'))) else '{}' end),
      case when typeof(json_extract(data, '$.fromModel.label')) = 'text' then json_object('label', json_extract(data, '$.fromModel.label')) else '{}' end)
    else NULL end))
    where json_valid(data) and json_type(data) = 'object'
      and ((json_extract(data, '$.type') = 'timeline' and json_extract(data, '$.timelineType') = 'model_change' and 'fromModel' in ('fromModel','toModel'))
        or (json_extract(data, '$.type') = 'subtask' and 'fromModel' = 'model'))
      and json_type(data, '$.fromModel') = 'object'
      and (json_type(data, '$.fromModel.providerID') is not null or json_type(data, '$.fromModel.modelID') is not null);
  update part set data = json_set(data, '$.toModelSelection', json(case when (typeof(json_extract(data, '$.toModel.providerID')) = 'text' and length(trim(json_extract(data, '$.toModel.providerID'))) > 0) and (typeof(json_extract(data, '$.toModel.modelID')) = 'text' and length(trim(json_extract(data, '$.toModel.modelID'))) > 0) then
    json_patch(
      json_patch(json_object('providerId', json_extract(data, '$.toModel.providerID'), 'modelId', json_extract(data, '$.toModel.modelID')),
        case when (typeof(json_extract(data, '$.toModel.variant')) = 'text' and length(trim(json_extract(data, '$.toModel.variant'))) > 0) then json_object('options', json_object('reasoningLevel', json_extract(data, '$.toModel.variant'))) else '{}' end),
      case when typeof(json_extract(data, '$.toModel.label')) = 'text' then json_object('label', json_extract(data, '$.toModel.label')) else '{}' end)
    else NULL end))
    where json_valid(data) and json_type(data) = 'object'
      and ((json_extract(data, '$.type') = 'timeline' and json_extract(data, '$.timelineType') = 'model_change' and 'toModel' in ('fromModel','toModel'))
        or (json_extract(data, '$.type') = 'subtask' and 'toModel' = 'model'))
      and json_type(data, '$.toModel') = 'object'
      and (json_type(data, '$.toModel.providerID') is not null or json_type(data, '$.toModel.modelID') is not null);
  update part set data = json_set(data, '$.modelSelection', json(case when (typeof(json_extract(data, '$.model.providerID')) = 'text' and length(trim(json_extract(data, '$.model.providerID'))) > 0) and (typeof(json_extract(data, '$.model.modelID')) = 'text' and length(trim(json_extract(data, '$.model.modelID'))) > 0) then
    json_patch(
      json_patch(json_object('providerId', json_extract(data, '$.model.providerID'), 'modelId', json_extract(data, '$.model.modelID')),
        case when (typeof(json_extract(data, '$.model.variant')) = 'text' and length(trim(json_extract(data, '$.model.variant'))) > 0) then json_object('options', json_object('reasoningLevel', json_extract(data, '$.model.variant'))) else '{}' end),
      case when typeof(json_extract(data, '$.model.label')) = 'text' then json_object('label', json_extract(data, '$.model.label')) else '{}' end)
    else NULL end))
    where json_valid(data) and json_type(data) = 'object'
      and ((json_extract(data, '$.type') = 'timeline' and json_extract(data, '$.timelineType') = 'model_change' and 'model' in ('fromModel','toModel'))
        or (json_extract(data, '$.type') = 'subtask' and 'model' = 'model'))
      and json_type(data, '$.model') = 'object'
      and (json_type(data, '$.model.providerID') is not null or json_type(data, '$.model.modelID') is not null);
