"""Listen audio for the tuya-local-avent card's background listen.

Serves a camera's sound alone, as an ADTS AAC stream (internet-radio
style), at /api/tuya_local_avent/listen/<camera entity id>. A phone's media
app plays plain audio on with the screen locked, where it pauses video.

Security: the view requires Home Assistant authentication and read access
to the camera entity. The card hands the media app a path signed with
`auth/sign_path`: short-lived, bound to the signing user's refresh token
(revoked with it), and only valid for this exact path. It goes through Home
Assistant's own URL (HTTPS, remote access), so nothing new is exposed.

Enable with `tuya_local_avent:` in configuration.yaml.
"""

from __future__ import annotations

import asyncio
import logging

from aiohttp import web

from homeassistant.auth.permissions.const import POLICY_READ
from homeassistant.components.camera import async_get_stream_source
from homeassistant.components.ffmpeg import get_ffmpeg_manager
from homeassistant.components.http import KEY_HASS_USER, HomeAssistantView
from homeassistant.core import HomeAssistant
from homeassistant.helpers import config_validation as cv
from homeassistant.helpers.typing import ConfigType

DOMAIN = "tuya_local_avent"
CONFIG_SCHEMA = cv.empty_config_schema(DOMAIN)
_LOGGER = logging.getLogger(__name__)

# Each listener is one ffmpeg reading the camera stream.
MAX_LISTENERS = 4
CHUNK = 4096


async def async_setup(hass: HomeAssistant, config: ConfigType) -> bool:
    """Register the listen view."""
    hass.http.register_view(ListenView(asyncio.Semaphore(MAX_LISTENERS)))
    return True


class ListenView(HomeAssistantView):
    """Audio-only stream of a camera."""

    url = "/api/tuya_local_avent/listen/{entity_id}"
    name = "api:tuya_local_avent:listen"
    requires_auth = True

    def __init__(self, slots: asyncio.Semaphore) -> None:
        self._slots = slots

    async def get(self, request: web.Request, entity_id: str) -> web.StreamResponse:
        """Stream the camera's first audio track, re-encoded to AAC."""
        hass: HomeAssistant = request.app["hass"]
        user = request[KEY_HASS_USER]
        if not entity_id.startswith("camera."):
            raise web.HTTPNotFound
        if not user.permissions.check_entity(entity_id, POLICY_READ):
            raise web.HTTPForbidden
        try:
            source = await async_get_stream_source(hass, entity_id)
        except Exception as err:  # noqa: BLE001 - unknown entity or camera error
            _LOGGER.debug("listen %s: no stream source: %s", entity_id, err)
            raise web.HTTPNotFound from err
        if not source:
            raise web.HTTPNotFound
        if self._slots.locked():
            raise web.HTTPServiceUnavailable(text="too many listeners")

        async with self._slots:
            proc = await asyncio.create_subprocess_exec(
                get_ffmpeg_manager(hass).binary,
                *("-v", "error", "-nostdin"),
                *(("-rtsp_transport", "tcp") if source.startswith("rtsp") else ()),
                *("-i", source),
                *("-map", "0:a:0", "-vn", "-sn", "-dn"),
                *("-c:a", "aac", "-b:a", "64k", "-ac", "1", "-f", "adts", "-"),
                stdin=asyncio.subprocess.DEVNULL,
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.DEVNULL,
            )
            resp = web.StreamResponse(headers={"Content-Type": "audio/aac", "Cache-Control": "no-store"})
            _LOGGER.info("listen %s: started for %s", entity_id, user.name)
            try:
                await resp.prepare(request)
                assert proc.stdout is not None
                while chunk := await proc.stdout.read(CHUNK):
                    await resp.write(chunk)
            except ConnectionResetError:
                pass  # the media app hung up (a cancel ends here too, via finally)
            finally:
                if proc.returncode is None:
                    proc.kill()
                await proc.wait()
                _LOGGER.info("listen %s: ended for %s", entity_id, user.name)
            return resp
