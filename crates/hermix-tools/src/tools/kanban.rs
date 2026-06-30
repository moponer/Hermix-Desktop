//! Kanban board tool — create boards, add/move/list cards.

use async_trait::async_trait;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use hermix_core::{tool_schema, traits::RiskLevel, JsonSchema, ToolError, ToolHandler, ToolSchema};

use std::collections::HashMap;
use std::sync::Arc;

use crate::ToolRegistry;

/// Register the kanban tool with the registry (P1.2 per-tool migration).
pub fn register(registry: &ToolRegistry) {
    let kanban_path = hermix_config::paths::hermix_home().join("kanban.json");
    let handler = Arc::new(KanbanHandler::new(Arc::new(FileKanbanBackend::new(kanban_path))));
    let schema = handler.schema();
    registry.register_with_risk(
        "kanban",
        "kanban",
        schema,
        handler,
        Arc::new(|| true),
        vec![],
        true,
        "Create boards, add/move/list cards",
        "📌",
        None,
        RiskLevel::Safe,
        false,
    );
}

// ---------------------------------------------------------------------------
// Data model
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Card {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub column: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Board {
    pub id: String,
    pub name: String,
    pub columns: Vec<String>,
    pub cards: Vec<Card>,
}

// ---------------------------------------------------------------------------
// KanbanBackend trait
// ---------------------------------------------------------------------------

#[async_trait]
pub trait KanbanBackend: Send + Sync {
    async fn create_board(&self, name: &str, columns: Vec<String>) -> Result<String, ToolError>;
    async fn add_card(
        &self,
        board_id: &str,
        column: &str,
        title: &str,
        description: Option<&str>,
    ) -> Result<String, ToolError>;
    async fn move_card(
        &self,
        board_id: &str,
        card_id: &str,
        to_column: &str,
    ) -> Result<String, ToolError>;
    async fn list_cards(&self, board_id: &str, column: Option<&str>) -> Result<String, ToolError>;
}

// ---------------------------------------------------------------------------
// FileKanbanBackend — JSON-file backed
// ---------------------------------------------------------------------------

pub struct FileKanbanBackend {
    path: std::path::PathBuf,
}

impl FileKanbanBackend {
    pub fn new(path: std::path::PathBuf) -> Self {
        Self { path }
    }

    fn load(&self) -> HashMap<String, Board> {
        std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn save(&self, boards: &HashMap<String, Board>) {
        let _ = std::fs::create_dir_all(self.path.parent().unwrap_or(std::path::Path::new(".")));
        if let Ok(json) = serde_json::to_string_pretty(boards) {
            let _ = std::fs::write(&self.path, json);
        }
    }
}

#[async_trait]
impl KanbanBackend for FileKanbanBackend {
    async fn create_board(&self, name: &str, columns: Vec<String>) -> Result<String, ToolError> {
        let mut boards = self.load();
        let id = format!("board-{}", boards.len() + 1);
        let board = Board {
            id: id.clone(),
            name: name.to_string(),
            columns,
            cards: vec![],
        };
        boards.insert(id.clone(), board);
        self.save(&boards);
        Ok(format!("Created board '{}' with id '{}'", name, id))
    }

    async fn add_card(
        &self,
        board_id: &str,
        column: &str,
        title: &str,
        description: Option<&str>,
    ) -> Result<String, ToolError> {
        let mut boards = self.load();
        let board_name;
        {
            let board = boards
                .get_mut(board_id)
                .ok_or_else(|| ToolError::InvalidParams(format!("Board '{}' not found", board_id)))?;

            if !board.columns.contains(&column.to_string()) {
                return Err(ToolError::InvalidParams(format!(
                    "Column '{}' not found in board. Available: {:?}",
                    column, board.columns
                )));
            }

            let card_id = format!("card-{}", board.cards.len() + 1);
            let card = Card {
                id: card_id.clone(),
                title: title.to_string(),
                description: description.map(|s| s.to_string()),
                column: column.to_string(),
            };
            board.cards.push(card);
            board_name = board.name.clone();
        }
        self.save(&boards);
        Ok(format!(
            "Added card '{}' to column '{}' in board '{}'",
            title, column, board_name
        ))
    }

    async fn move_card(
        &self,
        board_id: &str,
        card_id: &str,
        to_column: &str,
    ) -> Result<String, ToolError> {
        let mut boards = self.load();
        let (from, card_title, board_name);
        {
            let board = boards
                .get_mut(board_id)
                .ok_or_else(|| ToolError::InvalidParams(format!("Board '{}' not found", board_id)))?;

            if !board.columns.contains(&to_column.to_string()) {
                return Err(ToolError::InvalidParams(format!(
                    "Column '{}' not found in board. Available: {:?}",
                    to_column, board.columns
                )));
            }

            let card = board
                .cards
                .iter_mut()
                .find(|c| c.id == card_id)
                .ok_or_else(|| ToolError::InvalidParams(format!("Card '{}' not found", card_id)))?;

            from = card.column.clone();
            card_title = card.title.clone();
            card.column = to_column.to_string();
            board_name = board.name.clone();
        }
        self.save(&boards);
        Ok(format!(
            "Moved card '{}' from '{}' to '{}' in board '{}'",
            card_title, from, to_column, board_name
        ))
    }

    async fn list_cards(&self, board_id: &str, column: Option<&str>) -> Result<String, ToolError> {
        let boards = self.load();
        let board = boards
            .get(board_id)
            .ok_or_else(|| ToolError::InvalidParams(format!("Board '{}' not found", board_id)))?;

        let cards: Vec<&Card> = match column {
            Some(col) => board.cards.iter().filter(|c| c.column == col).collect(),
            None => board.cards.iter().collect(),
        };

        let result: Vec<Value> = cards
            .iter()
            .map(|c| {
                json!({
                    "id": c.id,
                    "title": c.title,
                    "description": c.description,
                    "column": c.column,
                })
            })
            .collect();

        serde_json::to_string_pretty(&result).map_err(|e| ToolError::ExecutionFailed(e.to_string()))
    }
}

// ---------------------------------------------------------------------------
// KanbanHandler
// ---------------------------------------------------------------------------

pub struct KanbanHandler {
    backend: Arc<dyn KanbanBackend>,
}

impl KanbanHandler {
    pub fn new(backend: Arc<dyn KanbanBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for KanbanHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'action' parameter".into()))?;

        match action {
            "create_board" => {
                let name = params
                    .get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'name' parameter".into()))?;
                let columns = params
                    .get("columns")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_else(|| vec!["To Do".into(), "In Progress".into(), "Done".into()]);
                self.backend.create_board(name, columns).await
            }
            "add_card" => {
                let board_id = params
                    .get("board_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        ToolError::InvalidParams("Missing 'board_id' parameter".into())
                    })?;
                let column = params
                    .get("column")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        ToolError::InvalidParams("Missing 'column' parameter".into())
                    })?;
                let title = params
                    .get("title")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'title' parameter".into()))?;
                let description = params.get("description").and_then(|v| v.as_str());
                self.backend
                    .add_card(board_id, column, title, description)
                    .await
            }
            "move_card" => {
                let board_id = params
                    .get("board_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        ToolError::InvalidParams("Missing 'board_id' parameter".into())
                    })?;
                let card_id = params
                    .get("card_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        ToolError::InvalidParams("Missing 'card_id' parameter".into())
                    })?;
                let to_column = params
                    .get("to_column")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        ToolError::InvalidParams("Missing 'to_column' parameter".into())
                    })?;
                self.backend.move_card(board_id, card_id, to_column).await
            }
            "list_cards" => {
                let board_id = params
                    .get("board_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        ToolError::InvalidParams("Missing 'board_id' parameter".into())
                    })?;
                let column = params.get("column").and_then(|v| v.as_str());
                self.backend.list_cards(board_id, column).await
            }
            other => Err(ToolError::InvalidParams(format!(
                "Unknown action: '{}'. Use 'create_board', 'add_card', 'move_card', or 'list_cards'.",
                other
            ))),
        }
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "action".into(),
            json!({
                "type": "string",
                "description": "Action: create_board, add_card, move_card, list_cards",
                "enum": ["create_board", "add_card", "move_card", "list_cards"]
            }),
        );
        props.insert(
            "name".into(),
            json!({
                "type": "string",
                "description": "Board name (for create_board)"
            }),
        );
        props.insert(
            "columns".into(),
            json!({
                "type": "array",
                "items": { "type": "string" },
                "description": "Column names (for create_board). Default: ['To Do', 'In Progress', 'Done']"
            }),
        );
        props.insert(
            "board_id".into(),
            json!({
                "type": "string",
                "description": "Board ID (for add_card, move_card, list_cards)"
            }),
        );
        props.insert(
            "column".into(),
            json!({
                "type": "string",
                "description": "Column name (for add_card, list_cards)"
            }),
        );
        props.insert(
            "title".into(),
            json!({
                "type": "string",
                "description": "Card title (for add_card)"
            }),
        );
        props.insert(
            "description".into(),
            json!({
                "type": "string",
                "description": "Card description (for add_card)"
            }),
        );
        props.insert(
            "card_id".into(),
            json!({
                "type": "string",
                "description": "Card ID (for move_card)"
            }),
        );
        props.insert(
            "to_column".into(),
            json!({
                "type": "string",
                "description": "Target column (for move_card)"
            }),
        );

        tool_schema(
            "kanban",
            "Kanban board: create boards, add cards, move cards between columns, list cards.",
            JsonSchema::object(props, vec!["action".into()]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    struct MockKanbanBackend;
    #[async_trait]
    impl KanbanBackend for MockKanbanBackend {
        async fn create_board(
            &self,
            name: &str,
            _columns: Vec<String>,
        ) -> Result<String, ToolError> {
            Ok(format!("Created board: {}", name))
        }
        async fn add_card(
            &self,
            _board_id: &str,
            _column: &str,
            title: &str,
            _description: Option<&str>,
        ) -> Result<String, ToolError> {
            Ok(format!("Added card: {}", title))
        }
        async fn move_card(
            &self,
            _board_id: &str,
            _card_id: &str,
            _to_column: &str,
        ) -> Result<String, ToolError> {
            Ok("Moved card".to_string())
        }
        async fn list_cards(
            &self,
            _board_id: &str,
            _column: Option<&str>,
        ) -> Result<String, ToolError> {
            Ok("[]".to_string())
        }
    }

    #[tokio::test]
    async fn test_kanban_create_board() {
        let handler = KanbanHandler::new(Arc::new(MockKanbanBackend));
        let result = handler
            .execute(json!({"action": "create_board", "name": "My Board"}))
            .await
            .unwrap();
        assert!(result.contains("Created board"));
    }

    #[tokio::test]
    async fn test_kanban_schema() {
        let handler = KanbanHandler::new(Arc::new(MockKanbanBackend));
        assert_eq!(handler.schema().name, "kanban");
    }
}
