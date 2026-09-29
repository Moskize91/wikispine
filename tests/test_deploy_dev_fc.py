from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path
from types import ModuleType
from unittest.mock import ANY, Mock, patch


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

    def test_enables_request_and_instance_logs(self) -> None:
        payload = deploy_dev_fc.build_update_payload(
            self.function(),
            image="registry/new:service-new",
            log_project="serverless-dev",
            logstore="default-logs",
        )
        self.assertEqual(
            payload["logConfig"],
            {
                "enableInstanceMetrics": True,
                "enableRequestMetrics": True,
                "logBeginRule": "None",
                "project": "serverless-dev",
                "logstore": "default-logs",
            },
        )

    def test_deploy_enables_fc_async_task_mode(self) -> None:
        function = self.function()
        function["functionName"] = "wikispine-dev"
        with (
            patch.object(deploy_dev_fc, "get_function", return_value=function),
            patch.object(deploy_dev_fc, "update_function"),
            patch.object(deploy_dev_fc, "wait_for_update", return_value=function),
            patch.object(
                deploy_dev_fc,
                "enable_async_tasks",
                return_value={"asyncTask": True},
            ) as enable_async_tasks,
        ):
            deploy_dev_fc.deploy(
                function_name="wikispine-dev",
                image="registry/new:service-new",
                log_project="serverless-dev",
                logstore="default-logs",
                region="ap-southeast-1",
                timeout_seconds=600,
                poll_seconds=0,
            )

        enable_async_tasks.assert_called_once_with(
            "wikispine-dev", region="ap-southeast-1", runner=ANY
        )

    def test_enables_fc_async_task_mode(self) -> None:
        calls: list[list[str]] = []

        def runner(arguments: list[str], **_: object):
            calls.append(arguments)
            return Mock(returncode=0, stdout='{"asyncTask":true}', stderr="")

        result = deploy_dev_fc.enable_async_tasks(
            "wikispine-dev", region="ap-southeast-1", runner=runner
        )

        self.assertTrue(result["asyncTask"])
        self.assertEqual(
            calls,
            [
                [
                    "aliyun",
                    "fc",
                    "PutAsyncInvokeConfig",
                    "--region",
                    "ap-southeast-1",
                    "--functionName",
                    "wikispine-dev",
                    "--body",
                    '{"asyncTask":true}',
                ]
            ],
        )


if __name__ == "__main__":
    unittest.main()
