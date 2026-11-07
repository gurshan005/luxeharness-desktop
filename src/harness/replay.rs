use super::*;

impl Engine {
    pub fn replay(&self, source: &RunRecord, body: Value) -> Result<String, String> {
        if source.spec.profile.provider.native() {
            return Err(
                "Native agents own their HTTP requests; use replay with an API-backed run.".into(),
            );
        }
        source.spec.profile.validate()?;
        if body["model"] != source.spec.profile.model
            || !body["messages"].is_array()
            || body["stream"] != true
        {
            return Err("Replay requires a captured streaming messages request with this connection's pinned model.".into());
        }
        if estimate(&body) + 4096 > source.spec.profile.context_tokens {
            return Err("Replay request exceeds the connection's context budget.".into());
        }
        let mut spec = source.spec.clone();
        spec.id = id();
        spec.task = format!("Replay of {}", source.spec.id);
        spec.mode = Mode::Plan;
        spec.isolation = Isolation::Shared;
        spec.parent = None;
        spec.check_only = false;
        spec.workspace = source.root.clone();
        let run = spec.id.clone();
        let control = Arc::new(Control::new(Mode::Plan));
        control.attach(
            self.shared.store.clone(),
            run.clone(),
            self.shared.events.clone(),
        )?;
        register(&self.shared, &spec, control.clone())?;
        let shared = self.shared.clone();
        let source_id = source.spec.id.clone();
        self.runtime.spawn(async move {
            let _lease=Lease{shared:shared.clone(),run:spec.id.clone()};
            let mut record=RunRecord{root:spec.workspace.clone(),spec,status:"Replaying captured request".into(),branch:None,rounds:0,context:estimate(&body),usage:Usage::default(),report:String::new(),transcript:vec![Turn::context("replay",format!("Experiment based on {source_id}. Tool requests are recorded as proposals and never executed."))],started:now(),native_session:None,replay_only:true};
            let result=async {
                publish(&shared,&record)?;
                trace(&shared,&record.spec.id,"replay_source",json!({"source_run":source_id,"tools_execute":false}))?;
                let profile=record.spec.profile.clone();
                let reply=call(&shared,&mut record,&body,control.clone(),0,&profile).await?;
                record.rounds=1;
                record.report=reply.turn.blocks.iter().filter_map(provider::block_text).collect::<Vec<_>>().join("\n");
                let proposals=reply.turn.blocks.iter().filter(|b|matches!(b,Block::ToolCall{..})).count();
                record.report.push_str(&format!("\n\nReplay finished ({}). {proposals} tool proposals were recorded; none executed.",reply.stop));
                record.transcript.push(reply.turn);
                Ok::<_,String>(())
            }.await;
            record.status=if control.stopped(){"Stopped"}else if result.is_ok(){"Completed"}else{"Failed"}.into();
            if let Err(error)=result {record.report=error;}
            let replay_id=record.spec.id.clone();
            if let Err(error)=publish(&shared,&record) {record.status="Failed".into();record.report=error;let _=shared.events.send(Event::Record(Box::new(record)));}
            let _=shared.events.send(Event::ResetStream(replay_id));
        });
        Ok(run)
    }
}
