"""Regression checks for publishing pinned RC images without rebuilding them."""

import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).with_name("distribution-publish-images.py")
spec = importlib.util.spec_from_file_location("distribution_publish_images", SCRIPT)
publisher = importlib.util.module_from_spec(spec)
spec.loader.exec_module(publisher)


class PublishImagesTest(unittest.TestCase):
    def setUp(self):
        self.source = "1" * 40
        self.digests = {variant: {platform: "sha256:" + str(number) * 64
                                  for number, platform in enumerate(publisher.PLATFORMS, 1)}
                        for variant in publisher.VARIANTS}
        for number, variant in enumerate(publisher.VARIANTS, 3):
            self.digests[variant]["linux/amd64"] = "sha256:" + str(number) * 64
            self.digests[variant]["linux/arm64"] = "sha256:" + str(number + 3) * 64

    def test_only_matching_rc_and_six_distinct_digests_are_accepted(self):
        publisher.validate_inputs("v2.9.0-rc.1", self.source, self.digests)
        for version in ("v2.9.0", "v2.9.0-rc.0", "v2.9.0-beta.1", "v2.9.0-rc.1;echo"):
            with self.subTest(version=version), self.assertRaisesRegex(ValueError, "RC version"):
                publisher.validate_inputs(version, self.source, self.digests)
        self.digests["op"]["linux/amd64"] = self.digests["combined"]["linux/amd64"]
        with self.assertRaisesRegex(ValueError, "distinct"):
            publisher.validate_inputs("v2.9.0-rc.1", self.source, self.digests)

    def test_staged_index_and_child_bytes_must_match_their_digests(self):
        child = {"mediaType": "application/vnd.oci.image.manifest.v1+json",
                 "config": {"digest": "sha256:" + "a" * 64}}
        raw_child = json.dumps(child).encode()
        child_digest = "sha256:" + hashlib.sha256(raw_child).hexdigest()
        index = {"mediaType": "application/vnd.oci.image.index.v1+json", "manifests": [
            {"mediaType": "application/vnd.oci.image.manifest.v1+json", "digest": child_digest,
             "platform": {"os": "linux", "architecture": "amd64"}}]}
        raw_index = json.dumps(index).encode()
        index_digest = "sha256:" + hashlib.sha256(raw_index).hexdigest()
        with patch.object(publisher, "command", side_effect=[raw_index, raw_child]):
            pins = publisher.platform_manifest("staging.example/repo", index_digest, "linux/amd64")
        self.assertEqual(pins, {"manifest_digest": child_digest, "config_digest": child["config"]["digest"]})
        with patch.object(publisher, "command", return_value=raw_index):
            with self.assertRaisesRegex(ValueError, "index does not match"):
                publisher.platform_manifest("staging.example/repo", "sha256:" + "b" * 64, "linux/amd64")

    def test_attestation_must_bind_the_staged_platform_manifest(self):
        raw_attestation = json.dumps({"mediaType": "application/vnd.oci.image.manifest.v1+json"}).encode()
        attestation = "sha256:" + hashlib.sha256(raw_attestation).hexdigest()
        platform_digest = "sha256:" + "a" * 64
        descriptor = {"digest": attestation, "annotations": {
            "vnd.docker.reference.type": "attestation-manifest",
            "vnd.docker.reference.digest": platform_digest}}
        raw_index = json.dumps({"mediaType": "application/vnd.oci.image.index.v1+json",
                                "manifests": [descriptor]}).encode()
        index_digest = "sha256:" + hashlib.sha256(raw_index).hexdigest()
        with patch.object(publisher, "command", side_effect=[raw_index, raw_attestation]):
            self.assertEqual(publisher.attestation_manifest("staging.example/repo",
                                                              index_digest, platform_digest), attestation)
        descriptor["annotations"]["vnd.docker.reference.digest"] = "sha256:" + "c" * 64
        raw_index = json.dumps({"mediaType": "application/vnd.oci.image.index.v1+json",
                                "manifests": [descriptor]}).encode()
        with patch.object(publisher, "command", return_value=raw_index):
            with self.assertRaisesRegex(ValueError, "bound attestation"):
                publisher.attestation_manifest("staging.example/repo",
                                               "sha256:" + hashlib.sha256(raw_index).hexdigest(), platform_digest)

    def test_published_attestations_must_remain_bound(self):
        repo = "ghcr.io/worldcoin/world-chain"
        reference = repo + ":combined-v2.9.0-rc.1"
        expected = {platform: {"manifest_digest": "sha256:" + str(i) * 64,
                               "config_digest": "sha256:" + str(i + 2) * 64}
                    for i, platform in enumerate(publisher.PLATFORMS, 1)}
        attestations = {platform: "sha256:" + str(i + 4) * 64
                        for i, platform in enumerate(publisher.PLATFORMS, 1)}
        descriptors = []
        records = {}
        for platform, pins in expected.items():
            descriptors.append({"platform": {"os": "linux", "architecture": platform.split("/")[1]},
                                "digest": pins["manifest_digest"]})
            records[repo + "@" + pins["manifest_digest"]] = (pins["manifest_digest"][7:],
                                                                {"config": {"digest": pins["config_digest"]}})
            digest = attestations[platform]
            descriptors.append({"platform": {"os": "unknown", "architecture": "unknown"},
                                "digest": digest, "annotations": {
                                    "vnd.docker.reference.type": "attestation-manifest",
                                    "vnd.docker.reference.digest": pins["manifest_digest"]}})
            records[repo + "@" + digest] = (digest[7:], {})
        records[reference] = ("f" * 64, {"mediaType": "application/vnd.oci.image.index.v1+json",
                                         "manifests": descriptors})
        with patch.object(publisher, "inspect", side_effect=records.__getitem__):
            self.assertEqual(publisher.verify_published(reference, expected, attestations), "sha256:" + "f" * 64)
            descriptors[1]["annotations"]["vnd.docker.reference.digest"] = "sha256:" + "0" * 64
            with self.assertRaisesRegex(ValueError, "unbound"):
                publisher.verify_published(reference, expected, attestations)

    def test_existing_different_tag_is_not_overwritten(self):
        with patch.object(publisher, "platform_manifest", return_value={"manifest_digest": "sha256:" + "a" * 64,
                                                                         "config_digest": "sha256:" + "b" * 64}), \
                patch.object(publisher, "attestation_manifest", return_value="sha256:" + "d" * 64), \
                patch.object(publisher, "verify_published", side_effect=ValueError("changed")), \
                patch.object(publisher, "command") as command:
            with self.assertRaisesRegex(ValueError, "changed"):
                publisher.publish("v2.9.0-rc.1", self.source, self.digests,
                                  "staging.example/repo", "ghcr.io/worldcoin/world-chain", Path("unused"))
            command.assert_not_called()

    def test_missing_tag_is_created_from_pinned_staging_digests(self):
        pins = {"manifest_digest": "sha256:" + "a" * 64, "config_digest": "sha256:" + "b" * 64}
        missing = subprocess.CalledProcessError(1, "inspect", stderr=b"manifest unknown")
        with patch.object(publisher, "platform_manifest", return_value=pins), \
                patch.object(publisher, "attestation_manifest", return_value="sha256:" + "d" * 64), \
                patch.object(publisher, "verify_published", side_effect=[missing, "sha256:" + "c" * 64] * 3), \
                patch.object(publisher, "command") as command, \
                patch.object(Path, "write_text"):
            publisher.publish("v2.9.0-rc.1", self.source, self.digests,
                              "staging.example/repo", "ghcr.io/worldcoin/world-chain", Path("unused"))
        self.assertEqual(command.call_count, 3)
        for call in command.call_args_list:
            self.assertEqual(call.args[:4], ("docker", "buildx", "imagetools", "create"))
            self.assertNotIn("latest", call.args)

    def test_matching_existing_tags_are_reused(self):
        pins = {"manifest_digest": "sha256:" + "a" * 64, "config_digest": "sha256:" + "b" * 64}
        with patch.object(publisher, "platform_manifest", return_value=pins), \
                patch.object(publisher, "attestation_manifest", return_value="sha256:" + "d" * 64), \
                patch.object(publisher, "verify_published", return_value="sha256:" + "c" * 64), \
                patch.object(publisher, "command") as command, \
                patch.object(Path, "write_text"):
            publisher.publish("v2.9.0-rc.1", self.source, self.digests,
                              "staging.example/repo", "ghcr.io/worldcoin/world-chain", Path("unused"))
            command.assert_not_called()


if __name__ == "__main__":
    unittest.main()
