use pinnacle_api_defs::pinnacle::{
    workspace::v1::{
        self, AddRequest, AddResponse, GetActiveRequest, GetActiveResponse, GetNameRequest,
        GetNameResponse, GetOutputNameRequest, GetOutputNameResponse, GetRequest, GetResponse,
        MoveToOutputRequest, MoveToOutputResponse, RemoveRequest, SetActiveRequest,
        SwitchToRequest,
    },
    util::v1::SetOrToggle,
};
use tonic::{Request, Status};

use crate::{
    api::{TonicResult, run_unary, run_unary_no_response},
    output::OutputName,
    state::WithState,
    workspace::WorkspaceId,
};

#[tonic::async_trait]
impl v1::workspace_service_server::WorkspaceService for super::WorkspaceService {
    async fn get(&self, _request: Request<GetRequest>) -> TonicResult<GetResponse> {
        run_unary(&self.sender, move |state| {
            let workspaces = state.pinnacle.outputs.iter().flat_map(|op| {
                op.with_state(|state| {
                    state
                        .workspaces
                        .iter()
                        .filter(|workspace| !workspace.defunct())
                        .cloned()
                        .collect::<Vec<_>>()
                })
            });

            let workspace_ids = workspaces.map(|workspace| workspace.id().to_inner()).collect();

            Ok(GetResponse { workspace_ids })
        })
        .await
    }

    async fn get_active(
        &self,
        request: Request<GetActiveRequest>,
    ) -> TonicResult<GetActiveResponse> {
        let workspace_id = WorkspaceId::new(request.into_inner().workspace_id);
        run_unary(&self.sender, move |state| {
            let active = workspace_id
                .workspace(&state.pinnacle)
                .map(|workspace| workspace.active())
                .unwrap_or_default();

            Ok(GetActiveResponse { active })
        })
        .await
    }

    async fn get_name(&self, request: Request<GetNameRequest>) -> TonicResult<GetNameResponse> {
        let workspace_id = WorkspaceId::new(request.into_inner().workspace_id);
        run_unary(&self.sender, move |state| {
            let name = workspace_id
                .workspace(&state.pinnacle)
                .map(|workspace| workspace.name())
                .unwrap_or_default();

            Ok(GetNameResponse { name })
        })
        .await
    }

    async fn get_output_name(
        &self,
        request: Request<GetOutputNameRequest>,
    ) -> TonicResult<GetOutputNameResponse> {
        let workspace_id = WorkspaceId::new(request.into_inner().workspace_id);
        run_unary(&self.sender, move |state| {
            let output_name = workspace_id
                .workspace(&state.pinnacle)
                .and_then(|workspace| Some(workspace.output(&state.pinnacle)?.name()))
                .unwrap_or_default();

            Ok(GetOutputNameResponse { output_name })
        })
        .await
    }

    async fn set_active(&self, request: Request<SetActiveRequest>) -> TonicResult<()> {
        let request = request.into_inner();

        let workspace_id = WorkspaceId::new(request.workspace_id);

        let set_or_toggle = request.set_or_toggle();

        if set_or_toggle == SetOrToggle::Unspecified {
            return Err(Status::invalid_argument("unspecified set or toggle"));
        }

        let active = match set_or_toggle {
            SetOrToggle::Set => Some(true),
            SetOrToggle::Unset => Some(false),
            SetOrToggle::Toggle => None,
            SetOrToggle::Unspecified => unreachable!(),
        };

        run_unary_no_response(&self.sender, move |state| {
            let Some(workspace) = workspace_id.workspace(&state.pinnacle) else {
                return;
            };

            crate::api::workspace::set_active(state, &workspace, active);
        })
        .await
    }

    async fn switch_to(&self, request: Request<SwitchToRequest>) -> TonicResult<()> {
        let request = request.into_inner();

        let workspace_id = WorkspaceId::new(request.workspace_id);

        run_unary_no_response(&self.sender, move |state| {
            let Some(workspace) = workspace_id.workspace(&state.pinnacle) else { return };
            crate::api::workspace::switch_to(state, &workspace);
        })
        .await
    }

    async fn add(&self, request: Request<AddRequest>) -> TonicResult<AddResponse> {
        let request = request.into_inner();

        let output_name = OutputName(request.output_name);

        let workspace_names = request.workspace_names;

        run_unary(&self.sender, move |state| {
            let workspaces = crate::api::workspace::add(state, workspace_names, output_name);

            let workspace_ids = workspaces.into_iter().map(|workspace| workspace.id().to_inner()).collect();

            Ok(AddResponse { workspace_ids })
        })
        .await
    }

    async fn move_to_output(
        &self,
        request: Request<MoveToOutputRequest>,
    ) -> TonicResult<MoveToOutputResponse> {
        let request = request.into_inner();

        let output_name = OutputName(request.output_name);

        let workspace_ids = request.workspace_ids.into_iter().map(WorkspaceId::new);

        run_unary(&self.sender, move |state| {
            let workspaces_to_move = workspace_ids
                .flat_map(|id| id.workspace(&state.pinnacle))
                .collect::<Vec<_>>();

            use crate::api::workspace::WorkspaceMoveToOutputError;
            use pinnacle_api_defs::pinnacle::workspace::v1::move_to_output_response::{
                Error,
                error::{Kind, SameWindowOnTwoOutputs},
            };

            let error = match crate::api::workspace::move_to_output(state, workspaces_to_move, output_name) {
                Ok(()) => None,
                Err(WorkspaceMoveToOutputError::OutputDoesNotExist) => Some(Error {
                    kind: Some(Kind::OutputDoesNotExist(())),
                }),
                Err(WorkspaceMoveToOutputError::SameWindowOnTwoOutputs(window_ids)) => Some(Error {
                    kind: Some(Kind::SameWindowOnTwoOutputs(SameWindowOnTwoOutputs {
                        window_ids: window_ids.into_iter().map(|id| id.0).collect(),
                    })),
                }),
            };
            Ok(MoveToOutputResponse { error })
        })
        .await
    }

    async fn remove(&self, request: Request<RemoveRequest>) -> TonicResult<()> {
        let request = request.into_inner();

        let workspace_ids = request.workspace_ids.into_iter().map(WorkspaceId::new);

        run_unary_no_response(&self.sender, move |state| {
            let workspaces_to_remove = workspace_ids
                .flat_map(|id| id.workspace(&state.pinnacle))
                .collect::<Vec<_>>();

            crate::api::workspace::remove(state, workspaces_to_remove);
        })
        .await
    }
}
