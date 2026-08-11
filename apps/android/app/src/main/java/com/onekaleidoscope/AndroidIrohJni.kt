package com.onekaleidoscope

import android.content.Context

/** Installs the process Android context before Rust can construct an iroh endpoint. */
internal object AndroidIrohJni {
    init {
        System.loadLibrary("kaleido_core")
    }

    external fun install(applicationContext: Context): Boolean
}
