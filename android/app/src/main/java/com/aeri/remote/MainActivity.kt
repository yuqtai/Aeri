package com.aeri.remote

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.viewModels
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.aeri.remote.ui.AeriTheme
import com.aeri.remote.ui.ControlScreen
import com.aeri.remote.ui.PairScreen
import com.aeri.remote.ui.pal

class MainActivity : ComponentActivity() {
    private val vm: MainViewModel by viewModels()

    override fun onCreate(savedInstanceState: Bundle?) {
        enableEdgeToEdge()
        super.onCreate(savedInstanceState)
        setContent { AeriTheme { App(vm) } }
    }

    // 界面在前台时才连着电脑，切到后台就断开（省电；电脑那边会自动松开说话键）
    override fun onStart() {
        super.onStart()
        vm.onStart()
    }

    override fun onStop() {
        vm.onStop()
        super.onStop()
    }
}

@Composable
private fun App(vm: MainViewModel) {
    val ui by vm.ui.collectAsStateWithLifecycle()
    val snackbar = remember { SnackbarHostState() }
    LaunchedEffect(Unit) {
        vm.notices.collect {
            snackbar.currentSnackbarData?.dismiss()
            snackbar.showSnackbar(it)
        }
    }
    Scaffold(
        containerColor = pal.canvas,
        contentWindowInsets = WindowInsets.safeDrawing,
        snackbarHost = { SnackbarHost(snackbar) },
    ) { padding ->
        if (ui.saved == null) {
            PairScreen(vm, Modifier.padding(padding))
        } else {
            ControlScreen(vm, ui, Modifier.padding(padding))
        }
    }
}
