use crate::infrai_client::{InfraiClient, InfraiError};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShipmentEventKind {
    PickedUp,
    InTransit,
    Delivered,
    Exception,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProofOfDelivery {
    pub object_key: String,
    pub signed_by: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShipmentEvent {
    pub shipment_id: String,
    pub kind: ShipmentEventKind,
    pub proof: Option<ProofOfDelivery>,
    pub exception_note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinRequest {
    pub email: String,
    pub name: String,
    pub company_domain: String,
    pub workspace_id: String,
    pub ownership_proof: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JoinDecision {
    Joined {
        user_id: String,
        workspace_id: String,
    },
    AwaitingDns {
        domain: String,
    },
}

#[derive(Debug)]
pub enum JoinError {
    InvalidEmailDomain,
    InvalidShipmentEvent(String),
    Infrai(InfraiError),
}

impl fmt::Display for JoinError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidEmailDomain => {
                write!(f, "employee email does not match the company domain")
            }
            Self::InvalidShipmentEvent(message) => write!(f, "invalid shipment event: {message}"),
            Self::Infrai(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for JoinError {}
impl From<InfraiError> for JoinError {
    fn from(value: InfraiError) -> Self {
        Self::Infrai(value)
    }
}

pub fn validate_shipment_event(event: &ShipmentEvent) -> Result<(), JoinError> {
    match event.kind {
        ShipmentEventKind::Delivered if event.proof.is_none() => Err(
            JoinError::InvalidShipmentEvent("delivery needs proof".into()),
        ),
        ShipmentEventKind::Exception
            if event.exception_note.as_deref().unwrap_or("").is_empty() =>
        {
            Err(JoinError::InvalidShipmentEvent(
                "exception needs a note".into(),
            ))
        }
        _ => Ok(()),
    }
}

pub async fn join_verified_employee(
    client: &InfraiClient,
    request: &JoinRequest,
) -> Result<JoinDecision, JoinError> {
    if email_domain(&request.email) != Some(request.company_domain.as_str()) {
        return Err(JoinError::InvalidEmailDomain);
    }
    let zone_id = client
        .add_domain(&request.company_domain, &request.workspace_id)
        .await?;
    client
        .upsert_ownership_txt(&zone_id, &request.ownership_proof)
        .await?;
    if !client.verify_domain(&request.company_domain).await? {
        return Ok(JoinDecision::AwaitingDns {
            domain: request.company_domain.clone(),
        });
    }
    let user_id = match client.find_user(&request.email).await? {
        Some(id) => id,
        None => {
            client
                .create_user(&request.email, &request.name, &request.workspace_id)
                .await?
        }
    };
    Ok(JoinDecision::Joined {
        user_id,
        workspace_id: request.workspace_id.clone(),
    })
}

fn email_domain(email: &str) -> Option<&str> {
    let (_, domain) = email.rsplit_once('@')?;
    (!domain.is_empty()).then_some(domain)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delivered_shipment_requires_proof_of_delivery() {
        let missing = ShipmentEvent {
            shipment_id: "SHP-42".into(),
            kind: ShipmentEventKind::Delivered,
            proof: None,
            exception_note: None,
        };
        assert!(matches!(
            validate_shipment_event(&missing),
            Err(JoinError::InvalidShipmentEvent(_))
        ));

        let signed = ShipmentEvent {
            shipment_id: "SHP-42".into(),
            kind: ShipmentEventKind::Delivered,
            proof: Some(ProofOfDelivery {
                object_key: "pod/SHP-42.pdf".into(),
                signed_by: "R. Chen".into(),
            }),
            exception_note: None,
        };
        assert!(validate_shipment_event(&signed).is_ok());
    }

    #[test]
    fn employee_domain_must_match_workspace_domain() {
        assert_eq!(
            email_domain("ops@northline.example"),
            Some("northline.example")
        );
        assert_ne!(
            email_domain("ops@carrier.example"),
            Some("northline.example")
        );
    }
}
