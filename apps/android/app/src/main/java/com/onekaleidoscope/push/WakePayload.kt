package com.onekaleidoscope.push

import java.nio.charset.StandardCharsets

/** Strict parser for the complete R4 data-only wake contract. */
internal object WakePayload {
    private val exactKeys = setOf("v", "kind", "route", "wake")
    private val opaqueId = Regex("[A-Za-z0-9_-]{22}")
    private const val MAX_CANONICAL_BYTES = 256

    fun validate(
        data: Map<String, String>,
        hasNotification: Boolean,
    ): WakePayloadValidation {
        if (hasNotification) return WakePayloadValidation.Rejected(WakePayloadRejection.Notification)
        if (data.keys != exactKeys) return WakePayloadValidation.Rejected(WakePayloadRejection.Keys)
        if (data["v"] != "1") return WakePayloadValidation.Rejected(WakePayloadRejection.Version)
        if (data["kind"] != "wake") return WakePayloadValidation.Rejected(WakePayloadRejection.Kind)
        val route = data["route"] ?: return WakePayloadValidation.Rejected(WakePayloadRejection.Keys)
        val wake = data["wake"] ?: return WakePayloadValidation.Rejected(WakePayloadRejection.Keys)
        if (!opaqueId.matches(route) || !opaqueId.matches(wake)) {
            return WakePayloadValidation.Rejected(WakePayloadRejection.OpaqueId)
        }
        val canonical = "{\"v\":\"1\",\"kind\":\"wake\",\"route\":\"$route\",\"wake\":\"$wake\"}"
        if (canonical.toByteArray(StandardCharsets.UTF_8).size > MAX_CANONICAL_BYTES) {
            return WakePayloadValidation.Rejected(WakePayloadRejection.Size)
        }
        return WakePayloadValidation.Accepted
    }
}

internal sealed interface WakePayloadValidation {
    data object Accepted : WakePayloadValidation
    data class Rejected(val reason: WakePayloadRejection) : WakePayloadValidation
}

internal enum class WakePayloadRejection { Notification, Keys, Version, Kind, OpaqueId, Size }
