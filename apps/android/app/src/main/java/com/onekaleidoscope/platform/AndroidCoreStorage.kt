package com.onekaleidoscope.platform

import android.content.Context
import java.io.File

/** Produces the only directory Android may pass to Rust's projection cache. */
object AndroidCoreStorage {
    fun projectionCacheDirectory(context: Context): File {
        val noBackupRoot = context.noBackupFilesDir.canonicalFile
        val cache = File(noBackupRoot, CACHE_DIRECTORY).canonicalFile
        val rootPrefix = noBackupRoot.path + File.separator
        check(cache.path.startsWith(rootPrefix)) {
            "projection cache escaped no-backup storage"
        }
        check(cache.isDirectory || cache.mkdirs()) {
            "projection cache directory is unavailable"
        }
        return cache
    }

    private const val CACHE_DIRECTORY = "projection-cache-v1"
}
