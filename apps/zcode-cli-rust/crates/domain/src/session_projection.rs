//! V4 projections of a session (state patch, snapshot and index summary).
use super::session::Session;
use serde_json::{Value, json};

impl Session {
    fn projected_title_source(&self) -> &str {
        // TS stored 身份有 first_input，V4 只有三值；与 product-projection 统一映射，避免冷恢复帧被拒绝。
        if self.title_source == "first_input" {
            "generated"
        } else {
            &self.title_source
        }
    }
    pub fn patch(&self) -> Value {
        let mut patch = json!({"revision":self.revision,
            "control":{"phase":self.phase,"sessionEnded":self.ended(),"canStop":self.running(),
                "stopState":if self.running(){"stoppable"}else{"idle"},"stopTargetKind":if self.running(){"mixed"}else{"unknown"},
                "activeWorks":self.run_id.as_ref().map(|id| vec![json!({"kind":"primaryTurn","foregroundExecutionId":id,"startedAt":self.updated_at})]).unwrap_or_default(),
                "lastError":self.last_error,"apiRetry":self.api_retry},
            "availability":{"fork":if self.history.responses.is_empty(){json!({"allowed":false,"reasonCode":"guard.forkTargetNotStable"})}else{json!({"allowed":true})},"compact":{"allowed":true},"switchModelConfig":{"allowed":true},
                "setFollowupMode":{"allowed":true},"queueEdit":{"allowed":true},"sendQueuedNow":if self.queued_now.is_none(){json!({"allowed":true})}else{json!({"allowed":false,"reasonCode":"guard.queuePromotionBusy"})},
                "pauseGoal":if self.goal.as_ref().is_some_and(|g|g.active()){json!({"allowed":true})}else{json!({"allowed":false,"reasonCode":if self.goal.is_none(){"noGoalToPause"}else{"goalNotActive"}})},
                "resumeGoal":if self.goal.as_ref().is_some_and(|g|matches!(g.status.as_str(),"paused"|"failed"|"notSatisfied")) && !self.running(){json!({"allowed":true})}else{json!({"allowed":false,"reasonCode":if self.goal.is_none(){"noGoalToResume"}else{"goalNotPaused"}})}},
            "inputRouting":{"mode":if self.running(){if self.followup_mode=="guide"{"guide"}else{"enqueue"}}else if !self.auto_drain && !self.queue.is_empty(){"choice"}else{"startNow"}},
            "meta":{"title":self.title,"titleSource":self.projected_title_source()},
            "config":{"provider":self.provider,"model":self.model,"thought":self.reasoning_level,"thoughtLevels":if self.thought_levels.is_empty(){vec![self.reasoning_level.clone()]}else{self.thought_levels.clone()},
                "modelSelection":{"providerId":self.provider,"modelId":self.model,"options":{"reasoningLevel":self.reasoning_level}},"followupMode":self.followup_mode,"mode":self.mode,"planEnabled":self.plan_enabled},
            "usage":self.usage,"queue":{"items":self.queue.iter().filter(|q|q["delivery"]["admitted"]!="startNow").collect::<Vec<_>>(),"autoDrain":self.auto_drain},
            "pendingInteractions":self.pending,"pendingCommands":[],"backgroundWorks":self.background.values().filter(|t|t.status=="running").map(|t|t.projection()).collect::<Vec<_>>(),"goal":self.goal.as_ref().map(|g|g.projection()),"plan":super::todo::plan(&self.todos,self.todos_updated_at)});
        if let Some(transition) = &self.plan_transition {
            patch["config"]["planTransition"] = transition.clone();
        }
        if let Some(interaction) = &self.permission_grant {
            patch["config"]["permissionGrant"] = json!({ "interactionId": interaction });
        }
        if self.provider.is_empty() || self.model.is_empty() {
            patch["config"]
                .as_object_mut()
                .unwrap()
                .remove("modelSelection");
        }
        if let Some(context) = &self.shared_context {
            patch["sharedContextImport"] = context.projection(&self.title);
        } else if self.legacy_shared_context {
            patch["sharedContextImport"] = json!({"title":self.title});
        }
        super::subagent::projection(self, &mut patch);
        patch
    }
    pub fn snapshot(&self) -> Value {
        let mut value = self.patch();
        let tail = self.rows.len().saturating_sub(60);
        value["protocolVersion"] = 1.into();
        value["sessionId"] = self.id.clone().into();
        value["logEpoch"] = self.epoch.clone().into();
        value["seq"] = self.seq.into();
        value["rows"] = json!({"window":self.rows[tail..],"totalCount":self.rows.len(),"firstRowId":self.rows.first().map(|v|&v["rowId"])});
        value
    }
    pub fn summary(&self) -> Value {
        let mut summary = json!({"sessionId":self.id,"workspaceId":self.workspace,"title":self.title,"titleSource":self.projected_title_source(),
            "phase":self.phase,"sessionEnded":self.ended(),"hasBackgroundWork":self.background.values().any(|t|t.status=="running"),"lastActivityAt":self.updated_at,"createdAt":self.created_at,
            "pendingInteractionSummary":{"permissionCount":self.pending.iter().filter(|p|p["kind"]=="permission").count(),"userInputCount":self.pending.iter().filter(|p|p["kind"]=="userInput").count()}});
        if let Some(p) = self.pending.first() {
            let mut interaction = json!({"interactionId":p["interactionId"],"kind":p["kind"]});
            if let Some(name) = p["payload"].get("toolName") {
                interaction["toolName"] = name.clone();
            }
            if let Some(a) = p.get("autoResolution") {
                interaction["autoResolution"] = a.clone();
            }
            summary["pendingInteraction"] = interaction;
        }
        if let Some(parent) = &self.parent_id {
            summary["parentSessionId"] = parent.clone().into();
        }
        if let Some(goal) = &self.goal {
            summary["goalStatus"] = goal.status.clone().into();
        }
        if self.children.values().any(|t| t.running()) {
            summary["hasBackgroundWork"] = true.into();
        }
        summary
    }
}
