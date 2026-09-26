from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path
from types import ModuleType


def load_deploy_module() -> ModuleType:
    path = Path(__file__).parents[1] / "scripts" / "deploy-dev-fc.py"
    spec = importlib.util.spec_from_file_location("deploy_dev_fc", path)
    if spec is None or spec.loader is None:
        raise RuntimeError("could not load deploy-dev-fc.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


deploy_dev_fc = load_deploy_module()


class BuildUpdatePayloadTest(unittest.TestCase):
    def function(self, *, acr_instance_id: str | None = None) -> dict[str, object]:
        return {
            "runtime": "custom-container",
            "customContainerConfig": {
                "acrInstanceId": acr_instance_id,
                "healthCheckConfig": {"httpGetUrl": "/healthz"},
                "image": "registry/old:service-old",
                "port": 9000,
                "registryConfig": {
                    "authConfig": None,
                    "authConfigEncrypted": None,
                    "certConfig": {"insecure": False},
                    "networkConfig": None,
                },
            },
        }

    def test_private_registry_credentials_are_written_without_acr_instance(self) -> None:
        payload = deploy_dev_fc.build_update_payload(
            self.function(),
            image="registry/new:service-new",
            registry_username="temporary-user",
            registry_password="temporary-password",
        )

        container = payload["customContainerConfig"]
        self.assertNotIn("acrInstanceId", container)
        self.assertEqual(container["image"], "registry/new:service-new")
        self.assertEqual(
            container["registryConfig"],
            {
                "authConfig": {
                    "userName": "temporary-user",
                    "password": "temporary-password",
                },
                "certConfig": {"insecure": False},
            },
        )

    def test_existing_acr_instance_is_preserved(self) -> None:
        payload = deploy_dev_fc.build_update_payload(
            self.function(acr_instance_id="cri-existing"),
            image="registry/new:service-new",
        )
        self.assertEqual(payload["customContainerConfig"]["acrInstanceId"], "cri-existing")

    def test_registry_credentials_must_be_provided_together(self) -> None:
        with self.assertRaisesRegex(deploy_dev_fc.DeploymentError, "provided together"):
            deploy_dev_fc.build_update_payload(
                self.function(),
                image="registry/new:service-new",
                registry_username="temporary-user",
            )


if __name__ == "__main__":
    unittest.main()
