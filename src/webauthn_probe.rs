use std::{collections::BTreeMap, fmt, time::Duration};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_cbor_2::Value as CborValue;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use url::Url;
use webauthn_rs_core::{
    WebauthnCore,
    proto::{
        AttestationFormat, AttestationMetadata, AuthenticationState, AuthenticatorTransport,
        COSEAlgorithm, COSEKey, COSEKeyType, Credential, ParsedAttestation, ParsedAttestationData,
        PublicKeyCredential, RegisterPublicKeyCredential, RegisteredExtensions, RegistrationState,
        RequestRegistrationExtensions, UserVerificationPolicy,
    },
};

const STATE_VERSION: u8 = 1;
const STATE_ENGINE: &str = "webauthn-rs-core-0.5.5";
const MAX_STATE_BYTES: usize = 65_536;
const MAX_PUBLIC_KEY_BYTES: usize = 8_192;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeError {
    InvalidConfiguration,
    InvalidPayload,
    InvalidState,
    StateConsumed,
    UnsupportedCredential,
    VerificationFailed,
}

impl fmt::Display for ProbeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidConfiguration => "invalid WebAuthn configuration",
            Self::InvalidPayload => "invalid WebAuthn payload",
            Self::InvalidState => "invalid WebAuthn ceremony state",
            Self::StateConsumed => "WebAuthn ceremony state was already consumed",
            Self::UnsupportedCredential => "unsupported WebAuthn credential",
            Self::VerificationFailed => "WebAuthn verification failed",
        })
    }
}

impl std::error::Error for ProbeError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeviceType {
    SingleDevice,
    MultiDevice,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredCredential {
    pub credential_id: String,
    pub public_key: Vec<u8>,
    pub counter: u32,
    pub device_type: DeviceType,
    pub backed_up: bool,
    pub transports: Vec<String>,
    pub aaguid: Option<[u8; 16]>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthenticationUpdate {
    pub counter: u32,
    pub device_type: DeviceType,
    pub backed_up: bool,
    pub user_verified: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExistingCredential {
    pub credential_id: String,
    pub transports: Vec<String>,
}

pub struct IssuedOptions {
    pub options: Value,
    pub state: CeremonyState,
    pub challenge_hash: [u8; 32],
}

pub struct CeremonyState {
    encoded: Option<Vec<u8>>,
}

impl CeremonyState {
    pub fn from_persisted(encoded: Vec<u8>) -> Result<Self, ProbeError> {
        validate_state_bytes(&encoded)?;
        Ok(Self {
            encoded: Some(encoded),
        })
    }

    pub fn persisted(&self) -> Result<&[u8], ProbeError> {
        self.encoded.as_deref().ok_or(ProbeError::StateConsumed)
    }

    fn consume(&mut self, expected: CeremonyKind) -> Result<CeremonyPayload, ProbeError> {
        let encoded = self.encoded.take().ok_or(ProbeError::StateConsumed)?;
        let envelope: StateEnvelope =
            serde_json::from_slice(&encoded).map_err(|_| ProbeError::InvalidState)?;
        if envelope.version != STATE_VERSION || envelope.engine != STATE_ENGINE {
            return Err(ProbeError::InvalidState);
        }
        if envelope.payload.kind() != expected {
            return Err(ProbeError::InvalidState);
        }
        Ok(envelope.payload)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CeremonyKind {
    Registration,
    Authentication,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StateEnvelope {
    version: u8,
    engine: String,
    payload: CeremonyPayload,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", content = "state", rename_all = "snake_case")]
enum CeremonyPayload {
    Registration(RegistrationState),
    Authentication(AuthenticationState),
}

impl CeremonyPayload {
    fn kind(&self) -> CeremonyKind {
        match self {
            Self::Registration(_) => CeremonyKind::Registration,
            Self::Authentication(_) => CeremonyKind::Authentication,
        }
    }
}

pub struct WebauthnProbe {
    core: WebauthnCore,
}

impl WebauthnProbe {
    pub fn new(
        rp_name: &str,
        rp_id: &str,
        origin: &str,
        timeout_millis: u64,
    ) -> Result<Self, ProbeError> {
        if rp_name.is_empty() || rp_id.is_empty() || timeout_millis == 0 {
            return Err(ProbeError::InvalidConfiguration);
        }
        u32::try_from(timeout_millis).map_err(|_| ProbeError::InvalidConfiguration)?;
        let parsed_origin = Url::parse(origin).map_err(|_| ProbeError::InvalidConfiguration)?;
        if parsed_origin.host_str() != Some(rp_id)
            || parsed_origin.path() != "/"
            || parsed_origin.query().is_some()
            || parsed_origin.fragment().is_some()
        {
            return Err(ProbeError::InvalidConfiguration);
        }
        Ok(Self {
            core: WebauthnCore::new_unsafe_experts_only(
                rp_name,
                rp_id,
                vec![parsed_origin],
                Duration::from_millis(timeout_millis),
                Some(false),
                Some(false),
            ),
        })
    }

    pub fn start_registration(
        &self,
        user_id: [u8; 16],
        user_id_text: &str,
        existing: &[ExistingCredential],
    ) -> Result<IssuedOptions, ProbeError> {
        let exclude_ids = existing
            .iter()
            .map(|credential| decode_credential_id(&credential.credential_id))
            .collect::<Result<Vec<_>, _>>()?;
        let extensions = RequestRegistrationExtensions {
            cred_protect: None,
            uvm: None,
            cred_props: Some(true),
            min_pin_length: None,
            hmac_create_secret: None,
        };
        let builder = self
            .core
            .new_challenge_register_builder(
                &user_id,
                &format!("admin:{user_id_text}"),
                "Histae administrator",
            )
            .map_err(|_| ProbeError::InvalidConfiguration)?
            .attestation(Default::default())
            .user_verification_policy(UserVerificationPolicy::Required)
            .exclude_credentials(Some(exclude_ids))
            .extensions(Some(extensions))
            .credential_algorithms(vec![
                COSEAlgorithm::EDDSA,
                COSEAlgorithm::ES256,
                COSEAlgorithm::RS256,
            ])
            .require_resident_key(true)
            .reject_synchronised_authenticators(false)
            .hints(Some(Vec::new()));
        let (generated, state) = self
            .core
            .generate_challenge_register(builder)
            .map_err(|_| ProbeError::InvalidConfiguration)?;
        let mut options =
            serde_json::to_value(generated.public_key).map_err(|_| ProbeError::InvalidState)?;
        attach_excluded_transports(&mut options, existing)?;
        issue(options, CeremonyPayload::Registration(state))
    }

    pub fn start_authentication(&self) -> Result<IssuedOptions, ProbeError> {
        let builder = self
            .core
            .new_challenge_authenticate_builder(Vec::new(), Some(UserVerificationPolicy::Required))
            .map_err(|_| ProbeError::InvalidConfiguration)?;
        let (generated, state) = self
            .core
            .generate_challenge_authenticate(builder)
            .map_err(|_| ProbeError::InvalidConfiguration)?;
        let mut options =
            serde_json::to_value(generated.public_key).map_err(|_| ProbeError::InvalidState)?;
        let object = options.as_object_mut().ok_or(ProbeError::InvalidState)?;
        object.remove("allowCredentials");
        issue(options, CeremonyPayload::Authentication(state))
    }

    pub fn finish_registration(
        &self,
        response: Value,
        state: &mut CeremonyState,
    ) -> Result<StoredCredential, ProbeError> {
        let payload = state.consume(CeremonyKind::Registration)?;
        validate_registration_payload(&response)?;
        let transports = registration_transports(&response)?;
        let aaguid = registration_aaguid(&response)?;
        let parsed: RegisterPublicKeyCredential =
            serde_json::from_value(response).map_err(|_| ProbeError::InvalidPayload)?;
        let CeremonyPayload::Registration(registration_state) = payload else {
            return Err(ProbeError::InvalidState);
        };
        let credential = self
            .core
            .register_credential(&parsed, &registration_state, None)
            .map_err(|_| ProbeError::VerificationFailed)?;
        if !credential.user_verified {
            return Err(ProbeError::VerificationFailed);
        }
        export_credential(&credential, transports, aaguid)
    }

    pub fn finish_authentication(
        &self,
        response: Value,
        stored: &StoredCredential,
        state: &mut CeremonyState,
    ) -> Result<AuthenticationUpdate, ProbeError> {
        validate_authentication_payload(&response)?;
        let current_backup_eligible = authentication_backup_eligible(&response)?;
        let parsed: PublicKeyCredential =
            serde_json::from_value(response).map_err(|_| ProbeError::InvalidPayload)?;
        if parsed.id != stored.credential_id {
            return Err(ProbeError::VerificationFailed);
        }
        let payload = state.consume(CeremonyKind::Authentication)?;
        let CeremonyPayload::Authentication(mut authentication_state) = payload else {
            return Err(ProbeError::InvalidState);
        };
        let credential = import_credential(stored, current_backup_eligible)?;
        authentication_state.set_allowed_credentials(vec![credential]);
        let result = self
            .core
            .authenticate_credential(&parsed, &authentication_state)
            .map_err(|_| ProbeError::VerificationFailed)?;
        if !result.user_verified() {
            return Err(ProbeError::VerificationFailed);
        }
        Ok(AuthenticationUpdate {
            counter: result.counter(),
            device_type: if result.backup_eligible() {
                DeviceType::MultiDevice
            } else {
                DeviceType::SingleDevice
            },
            backed_up: result.backup_state(),
            user_verified: result.user_verified(),
        })
    }
}

fn issue(options: Value, payload: CeremonyPayload) -> Result<IssuedOptions, ProbeError> {
    let challenge = options
        .get("challenge")
        .and_then(Value::as_str)
        .ok_or(ProbeError::InvalidState)?;
    let challenge_hash: [u8; 32] = Sha256::digest(challenge.as_bytes()).into();
    let encoded = serde_json::to_vec(&StateEnvelope {
        version: STATE_VERSION,
        engine: STATE_ENGINE.to_owned(),
        payload,
    })
    .map_err(|_| ProbeError::InvalidState)?;
    validate_state_bytes(&encoded)?;
    Ok(IssuedOptions {
        options,
        state: CeremonyState {
            encoded: Some(encoded),
        },
        challenge_hash,
    })
}

fn validate_state_bytes(encoded: &[u8]) -> Result<(), ProbeError> {
    if encoded.is_empty() || encoded.len() > MAX_STATE_BYTES {
        return Err(ProbeError::InvalidState);
    }
    let envelope: StateEnvelope =
        serde_json::from_slice(encoded).map_err(|_| ProbeError::InvalidState)?;
    if envelope.version != STATE_VERSION || envelope.engine != STATE_ENGINE {
        return Err(ProbeError::InvalidState);
    }
    Ok(())
}

fn attach_excluded_transports(
    options: &mut Value,
    existing: &[ExistingCredential],
) -> Result<(), ProbeError> {
    let exclusions = options
        .get_mut("excludeCredentials")
        .and_then(Value::as_array_mut)
        .ok_or(ProbeError::InvalidState)?;
    if exclusions.len() != existing.len() {
        return Err(ProbeError::InvalidState);
    }
    for (descriptor, credential) in exclusions.iter_mut().zip(existing) {
        validate_transports(&credential.transports)?;
        let object = descriptor.as_object_mut().ok_or(ProbeError::InvalidState)?;
        object.insert(
            "transports".to_owned(),
            Value::Array(
                credential
                    .transports
                    .iter()
                    .cloned()
                    .map(Value::String)
                    .collect(),
            ),
        );
    }
    Ok(())
}

fn export_credential(
    credential: &Credential,
    transports: Vec<String>,
    aaguid: Option<[u8; 16]>,
) -> Result<StoredCredential, ProbeError> {
    Ok(StoredCredential {
        credential_id: URL_SAFE_NO_PAD.encode(credential.cred_id.as_slice()),
        public_key: encode_cose_key(&credential.cred)?,
        counter: credential.counter,
        device_type: if credential.backup_eligible {
            DeviceType::MultiDevice
        } else {
            DeviceType::SingleDevice
        },
        backed_up: credential.backup_state,
        transports,
        aaguid,
    })
}

fn import_credential(
    stored: &StoredCredential,
    current_backup_eligible: bool,
) -> Result<Credential, ProbeError> {
    if stored.public_key.is_empty() || stored.public_key.len() > MAX_PUBLIC_KEY_BYTES {
        return Err(ProbeError::UnsupportedCredential);
    }
    let value: CborValue = serde_cbor_2::from_slice(&stored.public_key)
        .map_err(|_| ProbeError::UnsupportedCredential)?;
    let key = COSEKey::try_from(&value).map_err(|_| ProbeError::UnsupportedCredential)?;
    let credential_id = decode_credential_id(&stored.credential_id)?;
    Ok(Credential {
        cred_id: credential_id,
        cred: key,
        counter: stored.counter,
        transports: supported_transports(&stored.transports),
        user_verified: true,
        backup_eligible: current_backup_eligible,
        backup_state: stored.backed_up,
        registration_policy: UserVerificationPolicy::Required,
        extensions: RegisteredExtensions::none(),
        attestation: ParsedAttestation {
            data: ParsedAttestationData::None,
            metadata: AttestationMetadata::None,
        },
        attestation_format: AttestationFormat::None,
    })
}

fn encode_cose_key(key: &COSEKey) -> Result<Vec<u8>, ProbeError> {
    let mut fields = BTreeMap::new();
    let algorithm = key.type_ as i32 as i128;
    fields.insert(CborValue::Integer(3), CborValue::Integer(algorithm));
    match &key.key {
        COSEKeyType::EC_OKP(value) => {
            fields.insert(CborValue::Integer(1), CborValue::Integer(1));
            fields.insert(
                CborValue::Integer(-1),
                CborValue::Integer(value.curve.clone() as i128),
            );
            fields.insert(
                CborValue::Integer(-2),
                CborValue::Bytes(value.x.as_slice().to_vec()),
            );
        }
        COSEKeyType::EC_EC2(value) => {
            fields.insert(CborValue::Integer(1), CborValue::Integer(2));
            fields.insert(
                CborValue::Integer(-1),
                CborValue::Integer(value.curve.clone() as i128),
            );
            fields.insert(
                CborValue::Integer(-2),
                CborValue::Bytes(value.x.as_slice().to_vec()),
            );
            fields.insert(
                CborValue::Integer(-3),
                CborValue::Bytes(value.y.as_slice().to_vec()),
            );
        }
        COSEKeyType::RSA(value) => {
            fields.insert(CborValue::Integer(1), CborValue::Integer(3));
            fields.insert(
                CborValue::Integer(-1),
                CborValue::Bytes(value.n.as_slice().to_vec()),
            );
            fields.insert(CborValue::Integer(-2), CborValue::Bytes(value.e.to_vec()));
        }
    }
    serde_cbor_2::to_vec(&CborValue::Map(fields)).map_err(|_| ProbeError::UnsupportedCredential)
}

fn decode_credential_id(value: &str) -> Result<webauthn_rs_core::proto::CredentialID, ProbeError> {
    validate_encoded(value, 2_048)?;
    URL_SAFE_NO_PAD
        .decode(value)
        .map(Into::into)
        .map_err(|_| ProbeError::InvalidPayload)
}

fn supported_transports(values: &[String]) -> Option<Vec<AuthenticatorTransport>> {
    let transports = values
        .iter()
        .filter_map(|value| match value.as_str() {
            "ble" => Some(AuthenticatorTransport::Ble),
            "cable" | "hybrid" => Some(AuthenticatorTransport::Hybrid),
            "internal" => Some(AuthenticatorTransport::Internal),
            "nfc" => Some(AuthenticatorTransport::Nfc),
            "usb" => Some(AuthenticatorTransport::Usb),
            "smart-card" => None,
            _ => None,
        })
        .collect::<Vec<_>>();
    (!transports.is_empty()).then_some(transports)
}

fn registration_transports(response: &Value) -> Result<Vec<String>, ProbeError> {
    let Some(values) = response
        .get("response")
        .and_then(|value| value.get("transports"))
    else {
        return Ok(Vec::new());
    };
    let mut result = Vec::new();
    for value in values.as_array().ok_or(ProbeError::InvalidPayload)? {
        let transport = value.as_str().ok_or(ProbeError::InvalidPayload)?.to_owned();
        if !result.contains(&transport) {
            result.push(transport);
        }
    }
    Ok(result)
}

fn registration_aaguid(response: &Value) -> Result<Option<[u8; 16]>, ProbeError> {
    let encoded = response
        .get("response")
        .and_then(|value| value.get("attestationObject"))
        .and_then(Value::as_str)
        .ok_or(ProbeError::InvalidPayload)?;
    let attestation = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| ProbeError::InvalidPayload)?;
    let value: CborValue =
        serde_cbor_2::from_slice(&attestation).map_err(|_| ProbeError::InvalidPayload)?;
    let CborValue::Map(fields) = value else {
        return Err(ProbeError::InvalidPayload);
    };
    let auth_data = fields
        .get(&CborValue::Text("authData".to_owned()))
        .and_then(|value| match value {
            CborValue::Bytes(bytes) => Some(bytes.as_slice()),
            _ => None,
        })
        .ok_or(ProbeError::InvalidPayload)?;
    if auth_data.len() < 53 || auth_data[32] & 0x40 == 0 {
        return Err(ProbeError::InvalidPayload);
    }
    let mut aaguid = [0_u8; 16];
    aaguid.copy_from_slice(&auth_data[37..53]);
    Ok(Some(aaguid))
}

fn authentication_backup_eligible(response: &Value) -> Result<bool, ProbeError> {
    let encoded = response
        .get("response")
        .and_then(|value| value.get("authenticatorData"))
        .and_then(Value::as_str)
        .ok_or(ProbeError::InvalidPayload)?;
    let auth_data = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| ProbeError::InvalidPayload)?;
    if auth_data.len() < 37 {
        return Err(ProbeError::InvalidPayload);
    }
    Ok(auth_data[32] & 0x08 != 0)
}

pub fn validate_registration_payload(value: &Value) -> Result<(), ProbeError> {
    let object = credential_object(value)?;
    exact_keys(
        object,
        &[
            "id",
            "rawId",
            "type",
            "response",
            "authenticatorAttachment",
            "clientExtensionResults",
        ],
    )?;
    validate_common_credential(object)?;
    let response = object
        .get("response")
        .and_then(Value::as_object)
        .ok_or(ProbeError::InvalidPayload)?;
    exact_keys(
        response,
        &[
            "clientDataJSON",
            "attestationObject",
            "authenticatorData",
            "transports",
            "publicKeyAlgorithm",
            "publicKey",
        ],
    )?;
    validate_encoded_field(response, "clientDataJSON", 16_384, true)?;
    validate_encoded_field(response, "attestationObject", 131_072, true)?;
    validate_encoded_field(response, "authenticatorData", 16_384, false)?;
    validate_encoded_field(response, "publicKey", 16_384, false)?;
    if let Some(value) = response.get("publicKeyAlgorithm")
        && !value.is_i64()
        && !value.is_u64()
    {
        return Err(ProbeError::InvalidPayload);
    }
    if let Some(transports) = response.get("transports") {
        let values = transports.as_array().ok_or(ProbeError::InvalidPayload)?;
        if values.len() > 7 {
            return Err(ProbeError::InvalidPayload);
        }
        let values = values
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .ok_or(ProbeError::InvalidPayload)
            })
            .collect::<Result<Vec<_>, _>>()?;
        validate_transports(&values)?;
    }
    Ok(())
}

pub fn validate_authentication_payload(value: &Value) -> Result<(), ProbeError> {
    let object = credential_object(value)?;
    exact_keys(
        object,
        &[
            "id",
            "rawId",
            "type",
            "response",
            "authenticatorAttachment",
            "clientExtensionResults",
        ],
    )?;
    validate_common_credential(object)?;
    let response = object
        .get("response")
        .and_then(Value::as_object)
        .ok_or(ProbeError::InvalidPayload)?;
    exact_keys(
        response,
        &[
            "clientDataJSON",
            "authenticatorData",
            "signature",
            "userHandle",
        ],
    )?;
    validate_encoded_field(response, "clientDataJSON", 16_384, true)?;
    validate_encoded_field(response, "authenticatorData", 16_384, true)?;
    validate_encoded_field(response, "signature", 16_384, true)?;
    if !matches!(response.get("userHandle"), None | Some(Value::Null)) {
        validate_encoded_field(response, "userHandle", 2_048, true)?;
    }
    Ok(())
}

fn credential_object(value: &Value) -> Result<&Map<String, Value>, ProbeError> {
    value.as_object().ok_or(ProbeError::InvalidPayload)
}

fn validate_common_credential(object: &Map<String, Value>) -> Result<(), ProbeError> {
    let id = object
        .get("id")
        .and_then(Value::as_str)
        .ok_or(ProbeError::InvalidPayload)?;
    let raw_id = object
        .get("rawId")
        .and_then(Value::as_str)
        .ok_or(ProbeError::InvalidPayload)?;
    validate_encoded(id, 2_048)?;
    validate_encoded(raw_id, 2_048)?;
    if id != raw_id || object.get("type").and_then(Value::as_str) != Some("public-key") {
        return Err(ProbeError::InvalidPayload);
    }
    if let Some(attachment) = object.get("authenticatorAttachment")
        && !matches!(attachment.as_str(), Some("cross-platform" | "platform"))
    {
        return Err(ProbeError::InvalidPayload);
    }
    let extensions = object
        .get("clientExtensionResults")
        .and_then(Value::as_object)
        .ok_or(ProbeError::InvalidPayload)?;
    if serde_json::to_vec(extensions)
        .map_err(|_| ProbeError::InvalidPayload)?
        .len()
        > 8_192
    {
        return Err(ProbeError::InvalidPayload);
    }
    Ok(())
}

fn validate_encoded_field(
    object: &Map<String, Value>,
    key: &str,
    max_length: usize,
    required: bool,
) -> Result<(), ProbeError> {
    match object.get(key) {
        Some(Value::String(value)) => validate_encoded(value, max_length),
        None if !required => Ok(()),
        _ => Err(ProbeError::InvalidPayload),
    }
}

fn validate_encoded(value: &str, max_length: usize) -> Result<(), ProbeError> {
    if value.is_empty()
        || value.len() > max_length
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    {
        return Err(ProbeError::InvalidPayload);
    }
    Ok(())
}

fn exact_keys(object: &Map<String, Value>, allowed: &[&str]) -> Result<(), ProbeError> {
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(ProbeError::InvalidPayload);
    }
    Ok(())
}

fn validate_transports(values: &[String]) -> Result<(), ProbeError> {
    if values.len() > 7
        || values.iter().any(|value| {
            !matches!(
                value.as_str(),
                "ble" | "cable" | "hybrid" | "internal" | "nfc" | "smart-card" | "usb"
            )
        })
    {
        return Err(ProbeError::InvalidPayload);
    }
    Ok(())
}
