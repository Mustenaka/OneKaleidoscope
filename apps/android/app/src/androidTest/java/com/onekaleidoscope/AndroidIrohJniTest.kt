package com.onekaleidoscope

import android.app.Application
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class AndroidIrohJniTest {
    @Test
    fun applicationContextInstallationIsIdempotent() {
        val context = ApplicationProvider.getApplicationContext<Application>()

        assertTrue(AndroidIrohJni.install(context))
        assertTrue(AndroidIrohJni.install(context))
    }
}
