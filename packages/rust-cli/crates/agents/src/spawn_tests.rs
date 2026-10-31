//! 子代理的模型选择（工作线 F49）：团队项目跟父运行。

use rust_cli_base::models::{GLM_5_3_ACCOUNT, GLM_5_3_FLASH_ACCOUNT};
use rust_cli_kernel_api::model::{ModelConnection, ModelSelection};

use super::selection;

#[test]
fn a_child_keeps_the_team_project_of_the_parent() {
    let parent = ModelSelection {
        provider: "account:bigmodel-team-coding-plan".to_owned(),
        model: GLM_5_3_ACCOUNT.to_owned(),
        reasoning: Some("high".to_owned()),
        connection: Some(Box::new(ModelConnection {
            product_id: "prod-1".to_owned(),
            organization_id: "org-t".to_owned(),
            project_id: "p-t".to_owned(),
        })),
    };
    assert_eq!(selection(&parent, Some("inherit"), None), parent);
    let child = selection(&parent, Some(GLM_5_3_FLASH_ACCOUNT), Some("low".to_owned()));
    assert_eq!(child.model, GLM_5_3_FLASH_ACCOUNT);
    assert_eq!(child.reasoning.as_deref(), Some("low"));
    assert_eq!(child.connection, parent.connection);
}
