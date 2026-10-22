//! Keep an unmodified, pinned upstream implementation as an executable reference.
//! Fixed ephemeral keys are strictly for these deterministic test transcripts.
use snow as patched;
use upstream_snow as upstream;

macro_rules! transcript {
    ($backend:ident, $pattern:expr, $psk_slot:expr) => {{
        let initiator_key = [1; 32];
        let responder_key = [2; 32];
        let initiator_ephemeral = [3; 32];
        let responder_ephemeral = [4; 32];
        let psk = [0xa5; 32];
        let mut initiator = $backend::Builder::new($pattern.parse().unwrap())
            .local_private_key(&initiator_key)
            .unwrap()
            .fixed_ephemeral_key_for_testing_only(&initiator_ephemeral)
            .prologue(b"KVMFlow LAN control channel v1")
            .unwrap();
        let mut responder = $backend::Builder::new($pattern.parse().unwrap())
            .local_private_key(&responder_key)
            .unwrap()
            .fixed_ephemeral_key_for_testing_only(&responder_ephemeral)
            .prologue(b"KVMFlow LAN control channel v1")
            .unwrap();
        if let Some(slot) = $psk_slot {
            initiator = initiator.psk(slot, &psk).unwrap();
            responder = responder.psk(slot, &psk).unwrap();
        }
        let mut initiator = initiator.build_initiator().unwrap();
        let mut responder = responder.build_responder().unwrap();
        let mut trace = Vec::<Vec<u8>>::new();
        let mut frame = [0; 4096];
        let mut plaintext = [0; 4096];
        let mut turn = 0;
        while !initiator.is_handshake_finished() {
            let payload = format!("handshake payload {turn}");
            let (writer, reader) = if turn % 2 == 0 {
                (&mut initiator, &mut responder)
            } else {
                (&mut responder, &mut initiator)
            };
            let length = writer
                .write_message(payload.as_bytes(), &mut frame)
                .unwrap();
            trace.push(frame[..length].to_vec());
            let read = reader
                .read_message(&frame[..length], &mut plaintext)
                .unwrap();
            assert_eq!(&plaintext[..read], payload.as_bytes());
            trace.push(writer.get_handshake_hash().to_vec());
            trace.push(reader.get_handshake_hash().to_vec());
            turn += 1;
        }
        assert!(responder.is_handshake_finished());
        trace.push(initiator.get_remote_static().unwrap_or_default().to_vec());
        trace.push(responder.get_remote_static().unwrap_or_default().to_vec());
        let mut initiator = initiator.into_transport_mode().unwrap();
        let mut responder = responder.into_transport_mode().unwrap();
        for length in [0, 1, 63, 64, 65, 1024] {
            let payload = vec![0x5a; length];
            for backwards in [false, true] {
                let (writer, reader) = if backwards {
                    (&mut responder, &mut initiator)
                } else {
                    (&mut initiator, &mut responder)
                };
                let length = writer.write_message(&payload, &mut frame).unwrap();
                trace.push(frame[..length].to_vec());
                let read = reader
                    .read_message(&frame[..length], &mut plaintext)
                    .unwrap();
                assert_eq!(&plaintext[..read], payload.as_slice());
            }
        }
        trace
    }};
}

#[test]
fn xx_transcript_and_transport_match_unmodified_upstream() {
    let pattern = "Noise_XX_25519_ChaChaPoly_BLAKE2s";
    assert_eq!(
        transcript!(patched, pattern, None::<u8>),
        transcript!(upstream, pattern, None::<u8>)
    );
}

#[test]
fn psk_transcripts_and_transport_match_unmodified_upstream() {
    for (pattern, slot) in [
        ("Noise_NNpsk0_25519_ChaChaPoly_BLAKE2s", 0),
        ("Noise_XXpsk0_25519_ChaChaPoly_BLAKE2s", 0),
        ("Noise_XXpsk2_25519_ChaChaPoly_BLAKE2s", 2),
    ] {
        assert_eq!(
            transcript!(patched, pattern, Some(slot)),
            transcript!(upstream, pattern, Some(slot)),
            "{pattern}"
        );
    }
}
