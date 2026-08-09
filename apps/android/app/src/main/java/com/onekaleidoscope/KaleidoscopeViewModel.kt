package com.onekaleidoscope

import android.app.Application
import androidx.lifecycle.AndroidViewModel
import com.onekaleidoscope.data.MobileRepository
import com.onekaleidoscope.ui.UiAction

class KaleidoscopeViewModel(application: Application) : AndroidViewModel(application) {
    private val repository = MobileRepository(application)
    val state = repository.state

    fun dispatch(action: UiAction) = repository.dispatch(action)

    override fun onCleared() {
        repository.close()
        super.onCleared()
    }
}
