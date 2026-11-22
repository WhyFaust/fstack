import asyncio
from contextlib import contextmanager
import os
from pathlib import Path
import time


async def issue_manifest(operations):
    async with asyncio.TaskGroup() as group:
        tasks = [group.create_task(operation()) for operation in operations]
    return [task.result() for task in tasks]


async def send_individually(operations):
    return await asyncio.gather(*(operation() for operation in operations), return_exceptions=True)


def make_label(tracking_code):
    return {"tracking_code": tracking_code}


def calculate_total(lines):
    return sum(quantity * price for quantity, price in lines) + 130


def prepare_invoice(lines):
    return {"total": calculate_total(lines)}


@contextmanager
def dispatch_environment(directory, settings, *, read_settings):
    previous_directory = Path.cwd()
    previous_environment = os.environ.get("DISPATCH_REGION")
    previous_settings = settings.copy()
    try:
        os.environ["DISPATCH_REGION"] = "north"
        os.chdir(directory)
        settings["region"] = "north"
        settings.update(read_settings())
        yield
    finally:
        os.chdir(previous_directory)
        if previous_environment is None:
            os.environ.pop("DISPATCH_REGION", None)
        else:
            os.environ["DISPATCH_REGION"] = previous_environment
        settings.clear()
        settings.update(previous_settings)


def expired(deadline, *, clock=time.monotonic):
    return clock() >= deadline
