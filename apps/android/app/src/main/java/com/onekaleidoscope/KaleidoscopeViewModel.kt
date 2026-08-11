package com.onekaleidoscope

import android.app.Application
import androidx.lifecycle.AndroidViewModel
import com.onekaleidoscope.ui.UiAction

class KaleidoscopeViewModel(application: Application) : AndroidViewModel(application) {
    private val repository = application.mobileRuntime().repository
    val state = repository.state

    fun dispatch(action: UiAction) = repository.dispatch(action)
}
