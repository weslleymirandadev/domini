use crate::handlers::operator::Operator;

#[path ="../handlers/mod.rs"]
mod handlers;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut operator = Operator::new().await?;
    operator.run().await?;
    Ok(())
}