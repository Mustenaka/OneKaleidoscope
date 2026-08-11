package com.onekaleidoscope.ui

import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.NavigationBarItem
import androidx.compose.material3.NavigationRail
import androidx.compose.material3.NavigationRailItem
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.navigation.compose.NavHost
import androidx.navigation.compose.composable
import androidx.navigation.compose.currentBackStackEntryAsState
import androidx.navigation.compose.rememberNavController

@Composable
fun OneKaleidoscopeApp(
    state: AppUiState,
    onAction: (UiAction) -> Unit,
    modifier: Modifier = Modifier,
) {
    KaleidoscopeTheme {
        OneKaleidoscopeContent(state = state, onAction = onAction, modifier = modifier)
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun OneKaleidoscopeContent(
    state: AppUiState,
    onAction: (UiAction) -> Unit,
    modifier: Modifier,
) {
    val navController = rememberNavController()
    val backStackEntry by navController.currentBackStackEntryAsState()
    val currentRoute = backStackEntry?.destination?.route ?: Routes.Hosts
    val snackbar = remember { SnackbarHostState() }
    val dispatch: (UiAction) -> Unit = { action ->
        if (action is UiAction.Navigate) {
            navController.navigate(action.destination.route()) {
                launchSingleTop = true
                restoreState = true
            }
        }
        onAction(action)
    }

    LaunchedEffect(state.message?.id) {
        val message = state.message ?: return@LaunchedEffect
        val result = snackbar.showSnackbar(message.text, message.actionLabel)
        if (result == androidx.compose.material3.SnackbarResult.ActionPerformed) {
            onAction(UiAction.MessageAction)
        } else {
            onAction(UiAction.DismissMessage)
        }
    }

    BoxWithConstraints(modifier.fillMaxSize()) {
        val useRail = maxWidth >= 720.dp
        val appBody: @Composable (Modifier) -> Unit = { bodyModifier ->
            Scaffold(
                modifier = bodyModifier,
                topBar = {
                    TopAppBar(
                        title = {
                            Column {
                                Text(routeTitle(currentRoute), fontWeight = FontWeight.SemiBold)
                                Text(
                                    connectionLabel(state.connection),
                                    style = MaterialTheme.typography.labelSmall,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                )
                            }
                        },
                        navigationIcon = {
                            if (currentRoute == Routes.Sessions || currentRoute == Routes.Session) {
                                TextButton(onClick = { navController.popBackStack() }) { Text("返回") }
                            }
                        },
                    )
                },
                snackbarHost = { SnackbarHost(snackbar) },
                bottomBar = {
                    if (!useRail) {
                        AppBottomBar(currentRoute = currentRoute, onNavigate = { dispatch(UiAction.Navigate(it)) })
                    }
                },
            ) { padding ->
                AppNavHost(
                    state = state,
                    onAction = dispatch,
                    modifier = Modifier.fillMaxSize().padding(padding),
                    navController = navController,
                )
            }
        }

        if (useRail) {
            Row(Modifier.fillMaxSize()) {
                AppNavigationRail(currentRoute = currentRoute, onNavigate = { dispatch(UiAction.Navigate(it)) })
                appBody(Modifier.weight(1f))
            }
        } else {
            appBody(Modifier.fillMaxSize())
        }
    }
}

@Composable
private fun AppNavHost(
    state: AppUiState,
    onAction: (UiAction) -> Unit,
    modifier: Modifier,
    navController: androidx.navigation.NavHostController,
) {
    NavHost(navController = navController, startDestination = Routes.Hosts, modifier = modifier) {
        composable(Routes.Hosts) {
            HostsScreen(connection = state.connection, hosts = state.hosts, onAction = onAction)
        }
        composable(Routes.Projects) {
            ProjectsScreen(panel = state.projects, onAction = onAction)
        }
        composable(Routes.Sessions) {
            SessionsScreen(panel = state.sessions, onAction = onAction)
        }
        composable(Routes.Session) {
            Column(Modifier.fillMaxSize()) {
                SessionScreen(state = state, onAction = onAction, modifier = Modifier.weight(1f))
                PromptComposer(state = state, onAction = onAction)
            }
        }
        composable(Routes.Attention) {
            AttentionScreen(
                panel = state.attention,
                drafts = state.attentionDrafts,
                questionDrafts = state.questionDrafts,
                onAction = onAction,
            )
        }
        composable(Routes.Capabilities) {
            CapabilitiesScreen(panel = state.capabilities, onAction = onAction)
        }
    }
}

@Composable
private fun AppBottomBar(currentRoute: String, onNavigate: (AppDestination) -> Unit) {
    NavigationBar {
        primaryDestinations.forEach { destination ->
            NavigationBarItem(
                selected = currentRoute.belongsTo(destination),
                onClick = { onNavigate(destination) },
                icon = { Text(destination.shortLabel()) },
                label = { Text(destination.label()) },
            )
        }
    }
}

@Composable
private fun AppNavigationRail(currentRoute: String, onNavigate: (AppDestination) -> Unit) {
    NavigationRail {
        primaryDestinations.forEach { destination ->
            NavigationRailItem(
                selected = currentRoute.belongsTo(destination),
                onClick = { onNavigate(destination) },
                icon = { Text(destination.shortLabel()) },
                label = { Text(destination.label()) },
            )
        }
    }
}

private val primaryDestinations = listOf(
    AppDestination.Hosts,
    AppDestination.Projects,
    AppDestination.Attention,
    AppDestination.Capabilities,
)

private object Routes {
    const val Hosts = "hosts"
    const val Projects = "projects"
    const val Sessions = "sessions"
    const val Session = "session"
    const val Attention = "attention"
    const val Capabilities = "capabilities"
}

private fun AppDestination.route(): String = when (this) {
    AppDestination.Hosts -> Routes.Hosts
    AppDestination.Projects -> Routes.Projects
    AppDestination.Sessions -> Routes.Sessions
    AppDestination.Session -> Routes.Session
    AppDestination.Attention -> Routes.Attention
    AppDestination.Capabilities -> Routes.Capabilities
}

private fun AppDestination.label(): String = when (this) {
    AppDestination.Hosts -> "电脑"
    AppDestination.Projects -> "项目"
    AppDestination.Sessions -> "会话"
    AppDestination.Session -> "会话详情"
    AppDestination.Attention -> "待处理"
    AppDestination.Capabilities -> "能力"
}

private fun AppDestination.shortLabel(): String = when (this) {
    AppDestination.Hosts -> "PC"
    AppDestination.Projects -> "P"
    AppDestination.Sessions -> "S"
    AppDestination.Session -> "S"
    AppDestination.Attention -> "!"
    AppDestination.Capabilities -> "C"
}

private fun String.belongsTo(destination: AppDestination): Boolean = when (destination) {
    AppDestination.Hosts -> this == Routes.Hosts
    AppDestination.Projects -> this == Routes.Projects || this == Routes.Sessions || this == Routes.Session
    AppDestination.Attention -> this == Routes.Attention
    AppDestination.Capabilities -> this == Routes.Capabilities
    else -> false
}

private fun routeTitle(route: String): String = when (route) {
    Routes.Hosts -> "OneKaleidoscope"
    Routes.Projects -> "项目"
    Routes.Sessions -> "会话"
    Routes.Session -> "会话详情"
    Routes.Attention -> "待处理"
    Routes.Capabilities -> "运行时能力"
    else -> "OneKaleidoscope"
}

private fun connectionLabel(connection: ConnectionUiState): String = when (connection) {
    ConnectionUiState.Initializing -> "正在初始化"
    ConnectionUiState.Unpaired -> "未配对"
    is ConnectionUiState.Pairing -> "正在配对"
    is ConnectionUiState.Connecting -> "正在连接"
    is ConnectionUiState.Live -> "已连接：${connection.endpointLabel}"
    is ConnectionUiState.Offline -> "离线缓存"
    is ConnectionUiState.Revoked -> "设备已吊销"
    is ConnectionUiState.Error -> "连接错误"
}
