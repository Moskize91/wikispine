#!/usr/bin/env python3
"""Update the Singapore development FC function to a released image.

The function is read before updating so its existing container settings are
preserved. In particular, this avoids replacing the NAS-backed runtime data
configuration when only the image changes.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import tempfile
import time
from collections.abc import Callable, Sequence
from typing import Any


JsonObject = dict[str, Any]
CommandRunner = Callable[..., subprocess.CompletedProcess[str]]

_PRESERVED_CONTAINER_FIELDS = (
    "accelerationType",
    "command",
    "entrypoint",
    "healthCheckConfig",
    "port",
)


class DeploymentError(RuntimeError):
    """Raised when the target function cannot be safely updated."""


def _required(value: str | None, name: str) -> str:
    normalized = (value or "").strip()
    if not normalized:
        raise DeploymentError(f"{name} is required")
    return normalized


def get_function(
    function_name: str,
    *,
    region: str,
    runner: CommandRunner = subprocess.run,
) -> JsonObject:
    return _run_aliyun(
        ["fc", "GetFunction", "--region", region, "--functionName", function_name],
        runner=runner,
    )


def build_update_payload(
    function: JsonObject,
    *,
    image: str,
    acr_instance_id: str,
) -> JsonObject:
    if function.get("runtime") != "custom-container":
        raise DeploymentError("target function is not a custom-container function")

    current = function.get("customContainerConfig")
    if not isinstance(current, dict):
        raise DeploymentError("target function has no customContainerConfig")

    registry = current.get("registryConfig")
    if isinstance(registry, dict) and (registry.get("authConfig") or registry.get("authConfigEncrypted")):
        raise DeploymentError(
            "target function uses private registry credentials; refusing to replace "
            "customContainerConfig because FC does not return reusable plaintext credentials"
        )

    container = {
        name: current[name]
        for name in _PRESERVED_CONTAINER_FIELDS
        if current.get(name) is not None
    }
    container["image"] = _required(image, "image")
    container["acrInstanceId"] = _required(acr_instance_id, "acr_instance_id")
    return {"customContainerConfig": container}


def update_function(
    function_name: str,
    payload: JsonObject,
    *,
    region: str,
    runner: CommandRunner = subprocess.run,
) -> None:
    with tempfile.NamedTemporaryFile(mode="w", encoding="utf-8", suffix=".json") as body:
        json.dump(payload, body, separators=(",", ":"))
        body.flush()
        _run_aliyun(
            [
                "fc",
                "UpdateFunction",
                "--region",
                region,
                "--functionName",
                function_name,
                "--body-file",
                body.name,
            ],
            runner=runner,
        )


def wait_for_update(
    function_name: str,
    *,
    image: str,
    previous_modified_time: str | None,
    region: str,
    timeout_seconds: float,
    poll_seconds: float,
    runner: CommandRunner = subprocess.run,
    clock: Callable[[], float] = time.monotonic,
    sleep: Callable[[float], None] = time.sleep,
) -> JsonObject:
    deadline = clock() + timeout_seconds
    while True:
        function = get_function(function_name, region=region, runner=runner)
        status = function.get("lastUpdateStatus")
        reason = function.get("lastUpdateStatusReason") or function.get("stateReason")
        container = function.get("customContainerConfig") or {}
        current_image = container.get("image")
        resolved_image = container.get("resolvedImageUri")
        modified_time = function.get("lastModifiedTime")

        if status == "Failed":
            raise DeploymentError(f"FC rejected the image update: {reason or 'unknown reason'}")

        if current_image == image and status == "Successful":
            return function
        if (
            current_image == image
            and status is None
            and modified_time
            and modified_time != previous_modified_time
            and resolved_image
        ):
            return function

        if clock() >= deadline:
            raise DeploymentError(
                "timed out waiting for FC image update; "
                f"last status={status or 'unknown'}, current image={current_image!r}"
            )
        sleep(poll_seconds)


def deploy(
    *,
    function_name: str,
    image: str,
    acr_instance_id: str,
    region: str,
    timeout_seconds: float,
    poll_seconds: float,
    runner: CommandRunner = subprocess.run,
) -> JsonObject:
    current = get_function(function_name, region=region, runner=runner)
    if current.get("functionName") != function_name:
        raise DeploymentError("GetFunction returned a different function than requested")
    payload = build_update_payload(current, image=image, acr_instance_id=acr_instance_id)
    print(f"Updating FC function {function_name} to {image}")
    update_function(function_name, payload, region=region, runner=runner)
    return wait_for_update(
        function_name,
        image=image,
        previous_modified_time=current.get("lastModifiedTime"),
        region=region,
        timeout_seconds=timeout_seconds,
        poll_seconds=poll_seconds,
        runner=runner,
    )


def _run_aliyun(arguments: Sequence[str], *, runner: CommandRunner) -> JsonObject:
    result = runner(["aliyun", *arguments], capture_output=True, text=True, check=False)
    if result.returncode != 0:
        message = (result.stderr or result.stdout).strip()
        raise DeploymentError(f"Alibaba Cloud CLI failed ({result.returncode}): {message[:2000]}")
    try:
        output = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        raise DeploymentError("Alibaba Cloud CLI returned invalid JSON") from error
    if not isinstance(output, dict):
        raise DeploymentError("Alibaba Cloud CLI returned an unexpected JSON value")
    return output


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--function-name", required=True)
    parser.add_argument("--image", required=True)
    parser.add_argument("--acr-instance-id", required=True)
    parser.add_argument("--region", required=True)
    parser.add_argument("--timeout-seconds", type=float, default=600)
    parser.add_argument("--poll-seconds", type=float, default=5)
    return parser.parse_args()


def main() -> int:
    arguments = parse_args()
    try:
        deploy(
            function_name=arguments.function_name,
            image=arguments.image,
            acr_instance_id=arguments.acr_instance_id,
            region=arguments.region,
            timeout_seconds=arguments.timeout_seconds,
            poll_seconds=arguments.poll_seconds,
        )
    except (OSError, DeploymentError) as error:
        print(f"Error: {error}")
        return 1
    print(f"Development FC update completed for {arguments.function_name}.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
