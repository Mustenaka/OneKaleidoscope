package com.onekaleidoscope

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.runtime.getValue
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import com.onekaleidoscope.ui.OneKaleidoscopeApp

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        setContent {
            val model: KaleidoscopeViewModel = viewModel()
            val state by model.state.collectAsStateWithLifecycle()
            OneKaleidoscopeApp(state = state, onAction = model::dispatch)
        }
    }
}
