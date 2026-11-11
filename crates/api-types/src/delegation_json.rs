//! Concrete JSON adapters keep large delegation codecs out of HTTP handler
//! monomorphizations. Wire definitions remain in the protocol crate.
use sift_protocol::*;

pub trait DelegationWireJson: Sized {
    fn decode(bytes: &[u8]) -> serde_json::Result<Self>;
    fn encode(self) -> serde_json::Result<Vec<u8>>;
}

macro_rules! wire {
    ($($ty:ty),+ $(,)?) => {$(
        impl DelegationWireJson for $ty {
            #[inline(never)]
            fn decode(bytes: &[u8]) -> serde_json::Result<Self> {
                serde_json::from_slice(bytes)
            }
            #[inline(never)]
            fn encode(self) -> serde_json::Result<Vec<u8>> {
                serde_json::to_vec(&self)
            }
        }
    )+};
}
wire!(
    CreateAiDelegationReview,
    PreviewAiDelegationMaterialsRequest,
    ApproveAiDelegationReview,
    EndAiDelegationReview,
    SetAiDelegationTenantPolicy,
    AiDelegationTenantPolicy,
    AiDelegationReviewDetail,
    AiDelegationReviewSummary,
    Vec<AiDelegationReviewSummary>,
    AiDelegationMaterialsPreview,
);
